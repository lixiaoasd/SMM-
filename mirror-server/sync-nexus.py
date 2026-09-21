#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Nexus Mods 镜像同步（服务器版，移植自 sync-nexus.ps1）。

流程：updated.json 枚举 -> 每模组：元数据 -> files.json 选主文件 -> premium CDN 下载
-> 从 zip 内读 manifest.json -> 汇总写 public/index.json（按下载量排序）。
断点续传：磁盘已有 zip 直接复用；重跑自动跳过。

用法：
  python3 sync-nexus.py [--period 1m] [--workers 6] [--limit 0] [--max-file-kb 0]
参数：
  --period       1d | 1w | 1m（updated.json 窗口，默认 1m）
  --workers      并发线程数（默认 6）
  --limit        只处理前 N 个（0 = 全部）
  --max-file-kb  跳过超过该大小的文件（0 = 不限）
"""

import argparse
import json
import os
import re
import sys
import threading
import time
import uuid
import zipfile
from concurrent.futures import ThreadPoolExecutor, as_completed

import requests

API_BASE = "https://api.nexusmods.com/v1/games/stardewvalley"
MT_URL = "https://openspeech.bytedance.com/api/v3/machine_translation/matx_translate"
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
KEY_FILE = os.path.join(SCRIPT_DIR, "nexus-api.key")
MT_KEY_FILE = os.path.join(SCRIPT_DIR, "volc-mt.key")
CAT_FILE = os.path.join(SCRIPT_DIR, "categories.json")
FAILED_LOG = os.path.join(SCRIPT_DIR, "sync-failed.log")

# nginx 根目录；可用 --out-dir 覆盖
OUT_DIR_DEFAULT = "/var/www/mirror"
UA = "FireSVM-Mirror/1.0"

# 全局代理（Nexus 封大陆 IP 时用）。可通过 --proxy 或 HTTPS_PROXY 环境变量设置。
PROXIES = None

# 火山翻译：并发调用（服务端时延抖动大 0.15~27s/请求，串行会被慢请求堵死）。
# 4 路在途 + 提交间隔 0.1s 节流；TPM（每分钟 token）配额用全局冷却窗应对。
MT_KEY = None
MT_LAST_CALL = [0.0]
MT_THROTTLE_LOCK = threading.Lock()
MT_SEM = threading.BoundedSemaphore(4)
# TPM 限流全局冷却截止时间戳（撞 55000000 后所有 worker 一起等到窗口恢复）。
MT_TPM_UNTIL = [0.0]
# 单段最大字符。火山 MT 单条硬上限 1024 token：token 密度随内容变化
# （短词/URL/BBCode 多时实测 2200 字符可达 1221 token），1500 字符约
# 830 token 以下，双保险；仍超长由 mt_batch 二分/800 字符硬切兜底。
MT_CHUNK = 1500
# 单次 text_list 最多条数（慢路径下批内并行、时延≈最慢条，4 条兼顾吞吐与被拖慢概率）。
MT_BATCH = 4

# 致命错误标记（401/403 鉴权失败时中止整轮，不空耗）。
MT_FATAL = {"stop": False}


class MTError(Exception):
    """火山翻译调用失败；code 为业务码或 HTTPxxx；http 记录 HTTP 状态。"""

    def __init__(self, code, msg, http=0):
        super().__init__("MT code %s %s" % (code, str(msg)[:100]))
        self.code = code
        self.http = http


def nexus_get(url, api_key, stream=False, timeout=45):
    """GET with rate-limit handling (429/503 -> honor Retry-After; 5xx/network -> retry)."""
    headers = {"apikey": api_key, "User-Agent": UA}
    for _attempt in range(40):
        try:
            r = requests.get(url, headers=headers, timeout=timeout, stream=stream, proxies=PROXIES)
            if r.status_code in (429, 503):
                try:
                    ra = int(r.headers.get("Retry-After") or 30)
                except ValueError:
                    ra = 30
                time.sleep(min(ra, 1800) + 1)
                continue
            if r.status_code >= 500:
                time.sleep(5)
                continue
            if r.status_code >= 400:
                r.close()
                raise RuntimeError(f"HTTP {r.status_code}: {url}")
            return r
        except (requests.ConnectionError, requests.Timeout):
            time.sleep(5)
    raise RuntimeError("rate-limit/network retries exhausted")


def parse_manifest(zip_path):
    """Read manifest.json inside a zip; tolerate BOM/comments/trailing commas."""
    try:
        with zipfile.ZipFile(zip_path) as zf:
            for info in zf.infolist():
                name = info.filename.replace("\\", "/")
                if re.search(r"(^|/)manifest\.json$", name):
                    raw = zf.read(info).decode("utf-8-sig", errors="replace")
                    clean = re.sub(r"/\*.*?\*/", "", raw, flags=re.S)
                    clean = re.sub(r",\s*([}\]])", r"\1", clean)
                    try:
                        return json.loads(clean)
                    except json.JSONDecodeError:
                        return None
    except (zipfile.BadZipFile, OSError):
        return None
    return None


def pick_primary(files):
    """is_primary -> MAIN 最高 file_id -> 非 OLD_VERSION 最高 file_id。"""
    if not files:
        return None
    files = [f for f in files if isinstance(f, dict)]  # 容错：剔除 None/异常元素
    if not files:
        return None
    for f in files:
        if f.get("is_primary"):
            return f
    main = [f for f in files if f.get("category_name") == "MAIN"]
    if main:
        return max(main, key=lambda f: f.get("file_id", 0))
    rest = [f for f in files if f.get("category_name") != "OLD_VERSION"]
    if rest:
        return max(rest, key=lambda f: f.get("file_id", 0))
    return None


# ---------- 缩略图 ----------

def download_thumb(pic_url, mod_id, thumbs_dir):
    """下载模组封面到 thumbs/{mod_id}.{ext}；返回相对路径（失败返回空串）。"""
    if not pic_url:
        return ""
    m = re.search(r"\.(png|jpe?g|webp|gif)(?:\?|$)", pic_url, re.I)
    ext = ("." + m.group(1).lower().replace("jpeg", "jpg")) if m else ".png"
    rel = f"thumbs/{mod_id}{ext}"
    dest = os.path.join(thumbs_dir, f"{mod_id}{ext}")
    # 已存在任意同 id 扩展名的图即复用（避免主图换格式后留双份由回填脚本负责清理）。
    if os.path.exists(dest) and os.path.getsize(dest) > 0:
        return rel
    try:
        r = requests.get(pic_url, timeout=60, proxies=PROXIES)
        if r.status_code == 200 and r.content:
            tmp = dest + ".part"
            with open(tmp, "wb") as fh:
                fh.write(r.content)
            os.replace(tmp, dest)
            return rel
    except (requests.RequestException, OSError):
        pass
    return ""


# ---------- 火山引擎机器翻译（en -> zh） ----------

def _mt_call_once(texts):
    """单次调用；成功返回译文列表，失败抛 MTError。"""
    headers = {
        "Content-Type": "application/json",
        "x-api-key": MT_KEY,
        "X-Api-Resource-Id": "volc.speech.mt",
        "X-Api-Request-Id": uuid.uuid4().hex,
    }
    body = {"source_language": "en", "target_language": "zh", "text_list": texts}
    r = requests.post(MT_URL, headers=headers, json=body, timeout=60)
    if r.status_code != 200:
        raise MTError("HTTP%d" % r.status_code, r.text[:80], r.status_code)
    data = r.json()
    if data.get("code") != 20000000:
        raise MTError(data.get("code"), data.get("message"))
    tl = (data.get("data") or {}).get("translation_list") or []
    out = [str(x.get("translation") or "") for x in tl]
    if len(out) != len(texts):
        raise MTError("LEN", "result length mismatch")
    return out


def _mt_try(chunk):
    """节流 + 最多 4 路在途 + 重试（TPM 配额走全局冷却窗）。
    - 45000130（单条超 1024 token）不重试，立即抛出交上层切分；
    - 55000000（TPM 每分钟 token 超限）睡到下一整分钟窗口后重试；
    - 401/403 置致命标记后抛出；
    - 其他错误退避重试，耗尽后抛出。"""
    last_err = None
    tpm_waits = 0
    for attempt in range(8):
        # TPM 全局冷却（所有 worker 共用）。
        tpm_wait = MT_TPM_UNTIL[0] - time.time()
        if tpm_wait > 0:
            print("TPM cooldown %.0fs" % tpm_wait, flush=True)
            time.sleep(tpm_wait)
        # 提交节奏节流（sleep 在信号量外，不占并发槽）。
        with MT_THROTTLE_LOCK:
            wait = 0.1 - (time.time() - MT_LAST_CALL[0])
            if wait > 0:
                time.sleep(wait)
            MT_LAST_CALL[0] = time.time()
        t0 = time.time()
        try:
            with MT_SEM:
                out = _mt_call_once(chunk)
            dt = time.time() - t0
            if dt > 3.0:
                print("MT slow call: %.1fs segs=%d" % (dt, len(chunk)),
                      flush=True)
            return out
        except MTError as exc:
            last_err = exc
            if exc.code == 45000130:
                print("MT overflow -> split batch (segs=%d)" % len(chunk),
                      flush=True)
                raise
            if exc.code == 55000000:
                # 撞 TPM 配额墙：设全局冷却到下一整分钟边界 +2s；
                # 若离边界太近（疑似滑动窗口）则至少睡 20s。最多 4 轮。
                tpm_waits += 1
                if tpm_waits > 4:
                    raise
                now = time.time()
                boundary = (int(now) // 60 + 1) * 60 + 2
                if boundary - now < 5.0:
                    boundary = now + 20.0
                with MT_THROTTLE_LOCK:
                    if boundary > MT_TPM_UNTIL[0]:
                        MT_TPM_UNTIL[0] = boundary
                print("TPM limit hit, cool until %d (%.0fs)"
                      % (boundary, boundary - now), flush=True)
                continue
            print("MT retry %d/7 after %.2fs: %s segs=%d"
                  % (attempt + 1, time.time() - t0, exc, len(chunk)), flush=True)
            if exc.http in (401, 403):
                MT_FATAL["stop"] = True
                raise
            time.sleep(min(1.5 * (attempt + 1), 10.0))
    raise last_err


def _translate_chunk(chunk):
    """翻译一个批次（<=MT_BATCH 条），返回等长译文列表。
    超长自动二分；单条仍超则按 800 字符硬切后拼回；
    非致命失败对应位置降级为空串；致命鉴权错误向上抛 MTError。"""
    try:
        return _mt_try(chunk)
    except MTError as exc:
        if exc.code == 45000130:
            if len(chunk) > 1:
                mid = len(chunk) // 2
                left = _translate_chunk(chunk[:mid])
                right = _translate_chunk(chunk[mid:])
                return left + right
            # 单条仍超长：硬切（800 字符英文约 270 token，必然安全）。
            t = chunk[0]
            pieces = [t[i:i + 800] for i in range(0, len(t), 800)] or [""]
            return ["".join(_translate_chunk(pieces))]
        if MT_FATAL["stop"]:
            raise
        print("MT chunk failed (degrade to empty): %s" % exc, flush=True)
        return ["" for _ in chunk]


def mt_batch(texts):
    """批量翻译（节流+6 路在途+重试+超长自动切分）。
    同模组各片顺序调用；跨模组并发由调用方（回填线程池）提供，
    HTTP 在途总数由 MT_SEM 全局限制。仅鉴权等致命错误返回 None。"""
    if not MT_KEY or not any(t.strip() for t in texts):
        return None
    results = []
    for i in range(0, len(texts), MT_BATCH):
        try:
            results.extend(_translate_chunk(texts[i:i + MT_BATCH]))
        except MTError as exc:
            print("MT fatal, abort batch: %s" % exc, flush=True)
            return None
    return results


def _split_long(text, limit=MT_CHUNK):
    """长文本按换行/块标签边界切成 <=limit 的段，尽量不切断 BBCode。"""
    if len(text) <= limit:
        return [text]
    parts = []
    buf = ""
    # 优先在换行处断
    for line in text.split("\n"):
        while len(line) > limit:
            if buf:
                parts.append(buf)
                buf = ""
            parts.append(line[:limit])
            line = line[limit:]
        if len(buf) + len(line) + 1 > limit:
            parts.append(buf)
            buf = line
        else:
            buf = (buf + "\n" + line) if buf else line
    if buf:
        parts.append(buf)
    return [p for p in parts if p != ""] or [""]


def mt_fields(name, summary, desc):
    """翻译 name/summary/desc 三个字段；desc 自动分段。
    返回 (name_zh, summary_zh, desc_zh)，某字段空则返回空串；整体失败返回三个空串。"""
    segments = []   # (field, text)
    if name.strip():
        segments.append(("n", name.strip()))
    if summary.strip():
        segments.append(("s", summary.strip()))
    desc_parts = _split_long(desc or "")
    for p in desc_parts:
        if p.strip():
            segments.append(("d", p))
    if not segments:
        return "", "", ""
    translated = mt_batch([t for _, t in segments])
    if translated is None:
        return "", "", ""
    name_zh = summary_zh = ""
    desc_zh_parts = []
    for (field, _), zh in zip(segments, translated):
        if field == "n":
            name_zh = zh
        elif field == "s":
            summary_zh = zh
        else:
            desc_zh_parts.append(zh)
    return name_zh, summary_zh, "\n".join(desc_zh_parts)


def process_mod(mod_id, api_key, out_dir, max_file_kb, cat_map):
    """处理单个模组，返回 (status, entry_or_msg)。
    status: ok / skip / fail"""
    try:
        mods_dir = os.path.join(out_dir, "mods")
        thumbs_dir = os.path.join(out_dir, "thumbs")
        os.makedirs(thumbs_dir, exist_ok=True)
        # 1) 元数据
        m = nexus_get(f"{API_BASE}/mods/{mod_id}.json", api_key).json()
        if not m or not m.get("available") or m.get("status") in ("not_published", "removed"):
            return ("skip", None)

        # 2) 选主文件
        fo = nexus_get(f"{API_BASE}/mods/{mod_id}/files.json", api_key).json()
        if not fo:
            return ("fail", (mod_id, "empty files response"))
        primary = pick_primary(fo.get("files") or [])
        if primary is None:
            return ("fail", (mod_id, "no files"))
        fid = int(primary["file_id"])
        size_kb = int(primary.get("size_kb") or 0)
        if max_file_kb > 0 and size_kb > max_file_kb:
            return ("skip", None)

        fname = f"n{mod_id}-{fid}.zip"
        fpath = os.path.join(mods_dir, fname)
        if not (os.path.exists(fpath) and os.path.getsize(fpath) > 0):
            # 3) premium CDN 下载
            links = nexus_get(
                f"{API_BASE}/mods/{mod_id}/files/{fid}/download_link.json?key=premium",
                api_key,
            ).json()
            if not links or not links[0].get("URI"):
                return ("fail", (mod_id, "no cdn link"))
            tmp = fpath + ".part"
            with nexus_get(links[0]["URI"], api_key, stream=True, timeout=900) as r:
                with open(tmp, "wb") as fh:
                    for chunk in r.iter_content(chunk_size=256 * 1024):
                        if chunk:
                            fh.write(chunk)
            if not os.path.exists(tmp) or os.path.getsize(tmp) == 0:
                return ("fail", (mod_id, "empty download"))
            os.replace(tmp, fpath)

        # 4) 校验 zip + 读 manifest 还原真实身份
        manifest = parse_manifest(fpath) or {}  # zip 内可能无 manifest（如汉化包）
        cid = int(m.get("category_id") or 0)
        cat = cat_map.get(cid, {})
        # Nexus 页面信息：一句话简介 + BBCode 长描述 + 封面图。
        summary_en = str(m.get("summary") or "")
        desc_en = str(m.get("description") or "")
        thumb = download_thumb(str(m.get("picture_url") or ""), mod_id, thumbs_dir)
        name_en = str(manifest.get("Name") or m.get("name") or "")
        name_zh, summary_zh, desc_zh = mt_fields(name_en, summary_en, desc_en)
        entry = {
            "mod_id": int(mod_id),
            "name": name_en,
            "name_zh": name_zh,
            "author": str(manifest.get("Author") or (m.get("user") or {}).get("name") or ""),
            "version": str(manifest.get("Version") or m.get("version") or ""),
            "unique_id": str(manifest.get("UniqueID") or ""),
            "description": str(manifest.get("Description") or summary_en or ""),
            "summary": summary_en,
            "summary_zh": summary_zh,
            "desc": desc_en,
            "desc_zh": desc_zh,
            "thumb": thumb,
            "category_id": cid,
            "category": str(cat.get("zh", "")),
            "category_en": str(cat.get("en", "")),
            "file": f"mods/{fname}",
            "size": os.path.getsize(fpath),
            "_downloads": int(m.get("mod_downloads") or 0),
        }
        # SMAPI 前置/冲突：客户端据此做缺失前置自动补齐与冲突检测。
        deps = []
        d_raw = manifest.get("Dependencies") or []
        if isinstance(d_raw, dict):
            d_raw = [d_raw]
        for d in d_raw:
            if isinstance(d, dict):
                duid = str(d.get("UniqueID") or "").strip()
                if duid and d.get("IsRequired", True):
                    deps.append(duid)
            elif isinstance(d, str) and d.strip():
                deps.append(d.strip())
        entry["deps"] = deps
        c_raw = manifest.get("Conflicts") or []
        if isinstance(c_raw, str):
            c_raw = [c_raw]
        entry["confs"] = [str(c).strip() for c in c_raw if str(c).strip()]
        return ("ok", entry)
    except Exception as exc:  # noqa: BLE001 - 单个模组失败不影响整体
        return ("fail", (mod_id, str(exc)[:200]))


def load_existing(out_dir):
    """磁盘已有 zip 的条目直接复用（断点续传）。"""
    entries = []
    index_path = os.path.join(out_dir, "index.json")
    if not os.path.exists(index_path):
        return entries
    try:
        with open(index_path, "r", encoding="utf-8") as fh:
            old = json.load(fh)
        for e in old:
            fpath = os.path.join(out_dir, str(e.get("file", "")).replace("/", os.sep))
            if os.path.exists(fpath) and os.path.getsize(fpath) > 0:
                entries.append(e)
    except (json.JSONDecodeError, OSError):
        pass
    return entries


def write_index(out_dir, entries):
    """按下载量排序后原子写 index.json（nginx 读到的一定是完整文件）。"""
    entries = sorted(entries, key=lambda e: int(e.get("_downloads", 0)), reverse=True)
    tmp = os.path.join(out_dir, "index.json.tmp")
    final = os.path.join(out_dir, "index.json")
    with open(tmp, "w", encoding="utf-8") as fh:
        json.dump(entries, fh, ensure_ascii=False, separators=(",", ":"))
    os.replace(tmp, final)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--period", default="1m", choices=["1d", "1w", "1m"])
    ap.add_argument("--workers", type=int, default=6)
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--max-file-kb", type=int, default=0)
    ap.add_argument("--out-dir", default=OUT_DIR_DEFAULT)
    ap.add_argument("--proxy", default=None,
                    help="HTTP/SOCKS 代理，如 http://127.0.0.1:7890 或 socks5://127.0.0.1:7891")
    args = ap.parse_args()

    global PROXIES
    proxy = args.proxy or os.environ.get("HTTPS_PROXY") or os.environ.get("https_proxy")
    if proxy:
        PROXIES = {"http": proxy, "https": proxy}
        print(f"using proxy: {proxy}", flush=True)

    if not os.path.exists(KEY_FILE):
        print("missing nexus-api.key", file=sys.stderr)
        sys.exit(1)
    with open(KEY_FILE, "r", encoding="utf-8") as fh:
        api_key = fh.read().strip()

    global MT_KEY
    if os.path.exists(MT_KEY_FILE):
        with open(MT_KEY_FILE, "r", encoding="utf-8") as fh:
            MT_KEY = fh.read().strip()
        print("machine translation enabled (en->zh)", flush=True)
    else:
        print("volc-mt.key missing: zh fields will be empty", flush=True)

    cat_map = {}
    if os.path.exists(CAT_FILE):
        with open(CAT_FILE, "r", encoding="utf-8") as fh:
            cat_map = {int(k): v for k, v in json.load(fh).items()}

    mods_dir = os.path.join(args.out_dir, "mods")
    os.makedirs(mods_dir, exist_ok=True)

    # 1) 枚举目标模组
    print(f"fetching updated mod ids (period={args.period}) ...", flush=True)
    upd = nexus_get(f"{API_BASE}/mods/updated.json?period={args.period}", api_key).json()
    ids = [int(x["mod_id"]) for x in upd]
    if args.limit > 0:
        ids = ids[: args.limit]

    # 2) 断点续传：复用已有 zip
    entries = load_existing(args.out_dir)
    have = {int(e.get("mod_id", 0)) for e in entries}
    todo = [i for i in ids if i not in have]
    print(f"already mirrored: {len(entries)}; target: {len(ids)}; to process: {len(todo)}", flush=True)

    # 3) 并发处理
    done = failed = skipped = 0
    if os.path.exists(FAILED_LOG):
        os.remove(FAILED_LOG)
    if todo:
        with ThreadPoolExecutor(max_workers=args.workers) as pool:
            futures = {
                pool.submit(process_mod, i, api_key, args.out_dir, args.max_file_kb, cat_map): i
                for i in todo
            }
            last_write = 0
            for fut in as_completed(futures):
                status, payload = fut.result()
                if status == "ok":
                    entries.append(payload)
                    done += 1
                elif status == "skip":
                    skipped += 1
                else:
                    failed += 1
                    mod_id, msg = payload
                    with open(FAILED_LOG, "a", encoding="utf-8") as fh:
                        fh.write(f"{mod_id}\t{msg}\n")
                processed = done + failed + skipped
                if processed % 20 == 0 or processed == len(todo):
                    print(
                        f"progress: {processed}/{len(todo)}  ok={done} fail={failed} skip={skipped}",
                        flush=True,
                    )
                if done - last_write >= 10:
                    write_index(args.out_dir, entries)
                    last_write = done

    # 4) 最终 index
    write_index(args.out_dir, entries)
    total_mb = sum(int(e.get("size", 0)) for e in entries) / 1048576
    print(
        f"DONE: indexed={len(entries)} failed={failed} skipped={skipped} total={total_mb:.1f} MB",
        flush=True,
    )


if __name__ == "__main__":
    main()
