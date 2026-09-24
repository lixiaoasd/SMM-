//! 镜像源直装：从自建 HTTP 镜像（本地或 OSS/CDN）直接下载模组 zip，
//! 流式写入并实时回报真实进度（有 Content-Length），完成后自动安装到 Mods。
//!
//! 与 watch.rs 的区别：watch 监控浏览器下载（拿不到总大小，只有已下载 MB）；
//! mirror 是程序自己发 HTTP 请求，进度条有真实百分比，且完全不经过浏览器/风控。

use serde::Deserialize;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

// ---------- 清单 ----------

/// 镜像 index.json 中的一个模组条目。
#[derive(Debug, Clone, Deserialize)]
pub struct MirrorMod {
    /// N 网 mod id（合集按它收录；旧清单可能缺省为 0）。
    #[serde(default)]
    pub mod_id: i64,
    pub name: String,
    /// 机器翻译中文名（火山引擎，空串=暂无译文，客户端回退英文）。
    #[serde(default)]
    pub name_zh: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub version: String,
    /// manifest 里的 UniqueID，用于和本地已安装模组精确匹配。
    #[serde(default)]
    pub unique_id: String,
    /// manifest Dependencies 中必需前置的 UniqueID 列表（缺前置时客户端自动补齐）。
    #[serde(default)]
    pub deps: Vec<String>,
    /// manifest Conflicts 列表（SMAPI 正则，大小写不敏感匹配 UniqueID）。
    #[serde(default)]
    pub confs: Vec<String>,
    /// manifest Description（旧字段，兼容）。
    #[serde(default)]
    pub description: String,
    /// Nexus 一句话英文简介。
    #[serde(default)]
    pub summary: String,
    /// Nexus 一句话中文简介（机翻）。
    #[serde(default)]
    pub summary_zh: String,
    /// Nexus 页面 BBCode 英文长描述。
    #[serde(default)]
    pub desc: String,
    /// Nexus 页面 BBCode 中文长描述（机翻）。
    #[serde(default)]
    pub desc_zh: String,
    /// 封面缩略图相对路径，如 "thumbs/123.png"（空串=无图）。
    #[serde(default)]
    pub thumb: String,
    /// N 网分类 id（0 = 未知/未分类）。
    #[serde(default)]
    pub category_id: i64,
    /// 分类中文名（镜像 index.json 预填，空串=未知）。
    #[serde(default)]
    pub category: String,
    /// 分类英文名（N 网官方名，用于 tooltip）。
    #[serde(default)]
    pub category_en: String,
    /// 相对镜像根的 zip 路径，如 "mods/xxx-1.0.zip"。
    pub file: String,
    #[serde(default)]
    pub size: u64,
}

/// 拉取 `{base}/index.json`。
pub fn fetch_index(base: &str) -> Result<Vec<MirrorMod>, String> {
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("镜像源地址为空（设置页填写）".to_string());
    }
    let client = reqwest::blocking::Client::builder()
        .user_agent("StardewModManager")
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let url = format!("{base}/index.json");
    let resp = client
        .get(&url)
        .send()
        .map_err(|e| format!("无法连接镜像源：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("镜像源返回 HTTP {}", resp.status()));
    }
    // 直接从响应流解析，避免再多留一份 ~20MB 的中间 String。
    let list: Vec<MirrorMod> =
        serde_json::from_reader(resp).map_err(|e| format!("index.json 解析失败：{e}"))?;
    Ok(list)
}

/// 热门合集（跨分类的虚拟标签，如「新手必装」「大型扩展 DLC」）。
#[derive(Debug, Clone, Deserialize)]
pub struct MirrorCollection {
    pub id: String,
    pub zh: String,
    #[serde(default)]
    pub en: String,
    #[serde(default)]
    pub desc: String,
    /// 收录的 N 网 mod id 列表。
    #[serde(default)]
    pub mods: Vec<i64>,
}

/// 拉取 `{base}/collections.json`。旧镜像无此文件时返回 404 → 空列表，
/// 调用方应当作“没有合集功能”而非错误处理。
pub fn fetch_collections(base: &str) -> Result<Vec<MirrorCollection>, String> {
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() {
        return Ok(Vec::new());
    }
    let client = reqwest::blocking::Client::builder()
        .user_agent("StardewModManager")
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let url = format!("{base}/collections.json");
    let resp = client
        .get(&url)
        .send()
        .map_err(|e| format!("无法连接镜像源：{e}"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }
    if !resp.status().is_success() {
        return Err(format!("镜像源返回 HTTP {}", resp.status()));
    }
    let list: Vec<MirrorCollection> =
        serde_json::from_reader(resp).map_err(|e| format!("collections.json 解析失败：{e}"))?;
    Ok(list)
}

// ---------- 下载任务 ----------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MState {
    Downloading,
    Installing,
    Finished(bool, String),
}

pub struct MJob {
    pub id: u64,
    pub title: String,
    /// 去重键（镜像内文件路径）。
    key: String,
    pub state: MState,
    pub done: u64,
    pub total: u64,
    cancel: Arc<AtomicBool>,
}

impl MJob {
    pub fn is_active(&self) -> bool {
        !matches!(self.state, MState::Finished(..))
    }

    pub fn fraction(&self) -> Option<f32> {
        if self.total > 0 {
            Some((self.done as f32 / self.total as f32).clamp(0.0, 1.0))
        } else {
            None
        }
    }
}

struct Store {
    next_id: u64,
    jobs: Vec<MJob>,
}

static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
static MODS_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

fn store() -> &'static Mutex<Store> {
    STORE.get_or_init(|| {
        Mutex::new(Store {
            next_id: 1,
            jobs: Vec::new(),
        })
    })
}

pub fn set_mods_dir(dir: PathBuf) {
    *MODS_DIR.lock().unwrap() = Some(dir);
}

pub fn snapshot() -> Vec<MJobSnapshot> {
    store()
        .lock()
        .unwrap()
        .jobs
        .iter()
        .map(MJobSnapshot::from)
        .collect()
}

/// 快照（clone 出 UI 需要的纯数据，不含取消句柄）。
#[derive(Debug, Clone)]
pub struct MJobSnapshot {
    pub id: u64,
    pub title: String,
    /// 去重键（镜像内文件路径）。
    pub key: String,
    pub state: MState,
    pub done: u64,
    pub total: u64,
}

impl MJobSnapshot {
    pub fn is_active(&self) -> bool {
        !matches!(self.state, MState::Finished(..))
    }

    pub fn fraction(&self) -> Option<f32> {
        if self.total > 0 {
            Some((self.done as f32 / self.total as f32).clamp(0.0, 1.0))
        } else {
            None
        }
    }
}

impl From<&MJob> for MJobSnapshot {
    fn from(j: &MJob) -> Self {
        MJobSnapshot {
            id: j.id,
            title: j.title.clone(),
            key: j.key.clone(),
            state: j.state.clone(),
            done: j.done,
            total: j.total,
        }
    }
}

pub fn clear_finished() {
    store().lock().unwrap().jobs.retain(|j| j.is_active());
}

pub fn remove_job(id: u64) {
    store()
        .lock()
        .unwrap()
        .jobs
        .retain(|j| j.id != id || j.is_active());
}

pub fn cancel_job(id: u64) {
    if let Some(j) = store().lock().unwrap().jobs.iter().find(|j| j.id == id) {
        j.cancel.store(true, Ordering::SeqCst);
    }
}

/// 发起一个镜像模组的下载+安装（后台线程，立即返回）。
/// 同一文件已有活跃任务时忽略，避免重复下载。
pub fn download(base: String, mm: MirrorMod) {
    {
        let st = store().lock().unwrap();
        if st.jobs.iter().any(|j| j.is_active() && j.key == mm.file) {
            return;
        }
    }
    let (id, cancel) = {
        let mut st = store().lock().unwrap();
        let id = st.next_id;
        st.next_id += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        st.jobs.push(MJob {
            id,
            title: display_title(&mm),
            key: mm.file.clone(),
            state: MState::Downloading,
            done: 0,
            total: mm.size,
            cancel: cancel.clone(),
        });
        (id, cancel)
    };
    std::thread::spawn(move || {
        // 线程 panic 若不兜底，这条任务会永远停在「下载中」，行内按钮永久禁用。
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_job(id, cancel, base, mm)
        }));
        if r.is_err() {
            log_line(&format!("[{id}] 任务 panic，已标记为失败"));
            set_state(id, MState::Finished(false, "任务异常终止，请重试".to_string()));
        }
    });
}

fn display_title(mm: &MirrorMod) -> String {
    if mm.version.is_empty() {
        mm.name.clone()
    } else {
        format!("{} v{}", mm.name, mm.version)
    }
}

fn set_state(id: u64, state: MState) {
    if let Some(j) = store().lock().unwrap().jobs.iter_mut().find(|j| j.id == id) {
        j.state = state;
    }
}

fn set_title(id: u64, title: String) {
    if let Some(j) = store().lock().unwrap().jobs.iter_mut().find(|j| j.id == id) {
        j.title = title;
    }
}

fn run_job(id: u64, cancel: Arc<AtomicBool>, base: String, mm: MirrorMod) {
    let base = base.trim().trim_end_matches('/');
    let url = format!("{base}/{}", mm.file.trim_start_matches('/'));
    set_title(id, display_title(&mm));
    log_line(&format!("[{id}] GET {url}"));

    let mods_dir = match MODS_DIR.lock().unwrap().clone() {
        Some(d) => d,
        None => {
            let msg = "未配置 Mods 目录（设置页指定游戏路径）".to_string();
            log_line(&format!("[{id}] failed: {msg}"));
            set_state(id, MState::Finished(false, msg));
            return;
        }
    };

    // 暂存路径先算出来：下载中途失败也要能清理掉半截文件
    //（原来只在「成功」和「取消」两条路径删除，出错会把残包留在 mirror-downloads）。
    let dl_dir = crate::model::config_dir().join("mirror-downloads");
    let fname = mm
        .file
        .replace('/', "_")
        .replace('\\', "_")
        .trim_start_matches("mods_")
        .to_string();
    let tmp = dl_dir.join(if fname.ends_with(".zip") {
        fname
    } else {
        format!("{fname}.zip")
    });

    let downloaded = download_to_file(id, &cancel, &url, &dl_dir, &tmp, mm.size);
    let done = match downloaded {
        Ok(d) => d,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            log_line(&format!("[{id}] failed: {e}"));
            set_state(id, MState::Finished(false, e));
            return;
        }
    };
    if cancel.load(Ordering::SeqCst) {
        let _ = std::fs::remove_file(&tmp);
        set_state(id, MState::Finished(false, "已取消".to_string()));
        return;
    }

    set_state(id, MState::Installing);
    log_line(&format!("[{id}] downloaded {done} bytes, installing"));
    match crate::installer::install_archive(&tmp, &mods_dir) {
        Ok(names) => {
            let _ = std::fs::remove_file(&tmp);
            log_line(&format!("[{id}] installed: {}", names.join("、")));
            set_state(id, MState::Finished(true, format!("已安装：{}", names.join("、"))));
        }
        Err(e) => {
            // 安装失败保留压缩包，便于排查。
            let msg = format!("安装失败：{e}（压缩包已保留）");
            log_line(&format!("[{id}] failed: {msg}"));
            set_state(id, MState::Finished(false, msg));
        }
    }
}

/// 流式下载到 `tmp`，返回写出的字节数。
///
/// 出错时**不**清理文件，交给调用方在统一的失败路径删除（保证所有错误分支都清干净）。
fn download_to_file(
    id: u64,
    cancel: &AtomicBool,
    url: &str,
    dl_dir: &std::path::Path,
    tmp: &std::path::Path,
    fallback_total: u64,
) -> Result<u64, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("StardewModManager")
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(1800))
        .build()
        .map_err(|e| e.to_string())?;
    let mut resp = client
        .get(url)
        .send()
        .map_err(|e| format!("下载请求失败：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("镜像源返回 HTTP {}", resp.status()));
    }
    let total = resp.content_length().unwrap_or(fallback_total);
    if let Some(j) = store().lock().unwrap().jobs.iter_mut().find(|j| j.id == id) {
        j.total = total;
    }

    std::fs::create_dir_all(dl_dir).map_err(|e| e.to_string())?;
    let mut out = std::fs::File::create(tmp).map_err(|e| format!("创建临时文件失败：{e}"))?;

    // blocking::Response 实现了 Read：64KB 一块流式写，可随时取消。
    let mut buf = [0u8; 64 * 1024];
    let mut done: u64 = 0;
    let mut last_ui = Instant::now();
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err("已取消".to_string());
        }
        let n = match resp.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("读取数据流失败：{e}")),
        };
        out.write_all(&buf[..n]).map_err(|e| format!("写入失败：{e}"))?;
        done += n as u64;
        if last_ui.elapsed() >= Duration::from_millis(200) {
            if let Some(j) = store().lock().unwrap().jobs.iter_mut().find(|j| j.id == id) {
                j.done = done;
            }
            last_ui = Instant::now();
        }
    }
    out.flush().ok();
    // 必须在删除文件前释放句柄（Windows 下打开的文件无法删除）。
    drop(out);
    if let Some(j) = store().lock().unwrap().jobs.iter_mut().find(|j| j.id == id) {
        j.done = done;
    }
    Ok(done)
}

/// 诊断日志：%APPDATA%\StardewModManager\mirror.log（append）。
pub fn log_line(msg: &str) {
    let _ = (|| -> std::io::Result<()> {
        use std::io::Write;
        let path = crate::model::config_dir().join("mirror.log");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        writeln!(f, "{msg}")
    })();
}

// smoke 测试辅助：阻塞等待任务结束。
pub fn wait_finish(timeout: Duration, pred: impl Fn(&MJobSnapshot) -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if snapshot().iter().any(&pred) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    false
}
