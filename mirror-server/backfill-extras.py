#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""一次性回填：为现有 index.json 补齐封面缩略图 / Nexus 长描述 / 中文机翻。

阶段A（并发6）：拉 mods/{id}.json -> 写 summary/desc/thumb（缺啥补啥）
阶段B（串行限频）：火山翻译缺 name_zh 的条目
幂等：已有且文件存在的内容跳过，可随时中断重跑。

用法：python3 backfill-extras.py [--only A|B]
"""
import argparse
import json
import os
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed

sys.path.insert(0, "/opt/mirror")
import importlib.util  # noqa: E402
_spec = importlib.util.spec_from_file_location("sync_nexus", "/opt/mirror/sync-nexus.py")
sx = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(sx)

OUT = "/var/www/mirror"
INDEX = os.path.join(OUT, "index.json")
THUMBS = os.path.join(OUT, "thumbs")
# 冒烟模式（--limit）写独立文件，绝不覆盖全量 index.json。
SMOKE_INDEX = os.path.join(OUT, "index.smoke.json")
SMOKE = False


def load_index():
    with open(INDEX, encoding="utf-8") as f:
        return json.load(f)


def save_index(entries):
    entries.sort(key=lambda e: int(e.get("_downloads", 0)), reverse=True)
    target = SMOKE_INDEX if SMOKE else INDEX
    tmp = target + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(entries, f, ensure_ascii=False, separators=(",", ":"))
    os.replace(tmp, target)


def thumb_exists(rel):
    if not rel:
        return False
    p = os.path.join(OUT, rel.replace("/", os.sep))
    return os.path.exists(p) and os.path.getsize(p) > 0


def stage_a(entries, api_key):
    """并发：补 summary/desc/thumb。只处理缺失的条目。"""
    os.makedirs(THUMBS, exist_ok=True)

    def needs(e):
        return not e.get("summary") or not e.get("desc") or not thumb_exists(e.get("thumb"))

    todo = [e for e in entries if needs(e)]
    print("stage A: %d/%d entries need metadata/thumb" % (len(todo), len(entries)), flush=True)

    done = fail = 0

    def work(e):
        mid = int(e["mod_id"])
        try:
            m = sx.nexus_get("%s/mods/%d.json" % (sx.API_BASE, mid), api_key).json()
            if not m:
                return mid, None, "empty"
            summary = str(m.get("summary") or "")
            desc = str(m.get("description") or "")
            thumb = sx.download_thumb(str(m.get("picture_url") or ""), mid, THUMBS)
            return mid, (summary, desc, thumb), None
        except Exception as exc:  # noqa: BLE001
            return mid, None, str(exc)[:150]

    with ThreadPoolExecutor(max_workers=6) as pool:
        futs = {pool.submit(work, e): e for e in todo}
        for i, fut in enumerate(as_completed(futs)):
            mid, payload, err = fut.result()
            e = futs[fut]
            if payload:
                summary, desc, thumb = payload
                if summary:
                    e["summary"] = summary
                if desc:
                    e["desc"] = desc
                if thumb:
                    e["thumb"] = thumb
                done += 1
            else:
                fail += 1
                print("A fail mod %d: %s" % (mid, err), flush=True)
            if (i + 1) % 20 == 0:
                save_index(entries)
                print("A progress %d/%d ok=%d fail=%d" % (i + 1, len(todo), done, fail), flush=True)
    save_index(entries)
    print("stage A done: ok=%d fail=%d" % (done, fail), flush=True)


def _translate_one(e):
    """工作线程：翻译单个模组三字段（HTTP 并发由 sx.MT_SEM 全局限流 6 路）。"""
    name = str(e.get("name") or "")
    summary = str(e.get("summary") or e.get("description") or "")
    desc = str(e.get("desc") or "")
    return sx.mt_fields(name, summary, desc)


def stage_b(entries):
    """并发 4 路（受服务端 TPM 配额约束）：翻译缺中文的条目，可中断重跑。"""
    todo = [e for e in entries if not e.get("name_zh")]
    print("stage B: %d/%d entries need translation" % (len(todo), len(entries)), flush=True)
    if not sx.MT_KEY:
        print("MT key missing, abort B", flush=True)
        return
    ok = fail = 0
    t0 = time.time()
    pool = ThreadPoolExecutor(max_workers=4)
    futs = {pool.submit(_translate_one, e): e for e in todo}
    try:
        for i, fut in enumerate(as_completed(futs), start=1):
            e = futs[fut]
            try:
                nz, sz, dz = fut.result()
            except Exception as exc:  # noqa: BLE001
                print("worker error: %s" % str(exc)[:100], flush=True)
                nz = sz = dz = ""
            if nz:
                e["name_zh"] = nz
            if sz:
                e["summary_zh"] = sz
            if dz:
                e["desc_zh"] = dz
            if nz or sz or dz:
                ok += 1
            else:
                fail += 1
            if i % 10 == 0:
                save_index(entries)
                print("B progress %d/%d ok=%d fail=%d %.0fs"
                      % (i, len(todo), ok, fail, time.time() - t0), flush=True)
            if sx.MT_FATAL["stop"]:
                print("MT fatal (auth), cancelling remaining jobs", flush=True)
                pool.shutdown(wait=False, cancel_futures=True)
                break
    finally:
        pool.shutdown(wait=True)
    save_index(entries)
    print("stage B done: ok=%d fail=%d in %.0fs"
          % (ok, fail, time.time() - t0), flush=True)


def main():
    global SMOKE
    ap = argparse.ArgumentParser()
    ap.add_argument("--only", choices=["A", "B"], default=None)
    ap.add_argument("--limit", type=int, default=0, help="只处理前 N 个（冒烟测试）")
    args = ap.parse_args()

    key = open(sx.KEY_FILE, encoding="utf-8").read().strip()
    if os.path.exists(sx.MT_KEY_FILE):
        sx.MT_KEY = open(sx.MT_KEY_FILE, encoding="utf-8").read().strip()
        print("MT enabled", flush=True)

    entries = load_index()
    if args.limit > 0:
        global SMOKE
        SMOKE = True
        entries = entries[: args.limit]
        print("SMOKE mode: writing to index.smoke.json", flush=True)
    t0 = time.time()
    if args.only != "B":
        stage_a(entries, key)
    if args.only != "A":
        stage_b(entries)
    # 统计
    n_thumb = sum(1 for e in entries if thumb_exists(e.get("thumb")))
    n_zh = sum(1 for e in entries if e.get("name_zh"))
    n_desc = sum(1 for e in entries if e.get("desc"))
    print("ALL DONE in %.0fs: entries=%d thumb=%d zh=%d desc=%d"
          % (time.time() - t0, len(entries), n_thumb, n_zh, n_desc), flush=True)


if __name__ == "__main__":
    main()
