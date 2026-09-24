//! 浏览器下载监控（免费账号最稳的下载路径）。
//!
//! 用户在系统浏览器里正常下载 N 网文件（Manual Download → Slow Download），
//! 浏览器把 zip 下到系统「下载」文件夹；本模块监控该文件夹，
//! 发现新的 zip 自动校验（含 manifest.json）并安装到 Mods，成功后清理压缩包。
//!
//! - 只处理「监控会话」启动之后新出现的文件，不动旧文件；
//! - 跟踪 `.crdownload`（Edge/Chrome）/`.part`（Firefox）显示实时进度；
//! - 不是 SMAPI 模组的 zip 一律忽略并保留原文件。

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use zip::ZipArchive;

// ---------- 数据结构 ----------

/// 监控任务状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WState {
    /// 浏览器下载中（done = 已写入的字节数）。
    Downloading,
    /// 正在校验并安装。
    Installing,
    /// 已结束（bool=是否成功，String=详情）。
    Finished(bool, String),
}

/// UI 可见的监控任务。
#[derive(Debug, Clone)]
pub struct WatchJob {
    pub id: u64,
    /// 展示名（最终 zip 文件名）。
    pub title: String,
    /// 完整路径（能推导出来时）。
    pub path: Option<PathBuf>,
    pub state: WState,
    pub done: u64,
}

impl WatchJob {
    pub fn is_active(&self) -> bool {
        !matches!(self.state, WState::Finished(..))
    }
}

struct Store {
    next_id: u64,
    jobs: Vec<WatchJob>,
    /// 已完成、待安装的 job id（串行安装）。
    queue: VecDeque<u64>,
    /// zip 稳定性观察：路径 -> (上次大小, 上次变化时刻)。
    watch_zip: HashMap<PathBuf, (u64, Instant)>,
    /// 会话启动时已存在的文件名（不碰旧文件）。
    baseline: HashSet<String>,
}

static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
static SESSION: AtomicBool = AtomicBool::new(false);
static THREAD_SPAWNED: AtomicBool = AtomicBool::new(false);
static MODS_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

fn store() -> &'static Mutex<Store> {
    STORE.get_or_init(|| {
        Mutex::new(Store {
            next_id: 1,
            jobs: Vec::new(),
            queue: VecDeque::new(),
            watch_zip: HashMap::new(),
            baseline: HashSet::new(),
        })
    })
}

// ---------- 对外 API ----------

pub fn set_mods_dir(dir: PathBuf) {
    *MODS_DIR.lock().unwrap() = Some(dir);
}

pub fn session_active() -> bool {
    SESSION.load(Ordering::SeqCst)
}

/// 开启监控会话（幂等）。
pub fn start_session() {
    {
        let mut st = store().lock().unwrap();
        st.baseline = downloads_dir()
            .and_then(|d| std::fs::read_dir(d).ok())
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_ascii_lowercase())
                    .collect()
            })
            .unwrap_or_default();
        st.watch_zip.clear();
    }
    SESSION.store(true, Ordering::SeqCst);
    log_line("监控会话已开启");
    if !THREAD_SPAWNED.swap(true, Ordering::SeqCst) {
        std::thread::spawn(watcher_loop);
    }
}

/// 停止监控；进行中的任务标记为已停止。
pub fn stop_session() {
    SESSION.store(false, Ordering::SeqCst);
    let mut st = store().lock().unwrap();
    for j in st.jobs.iter_mut() {
        if j.is_active() {
            j.state = WState::Finished(false, "监控已停止".to_string());
        }
    }
    st.queue.clear();
    st.watch_zip.clear();
    log_line("监控会话已停止");
}

pub fn snapshot() -> Vec<WatchJob> {
    store().lock().unwrap().jobs.clone()
}

pub fn clear_finished() {
    store().lock().unwrap().jobs.retain(|j| j.is_active());
}

/// 移除一条已结束的记录。
pub fn remove_job(id: u64) {
    store()
        .lock()
        .unwrap()
        .jobs
        .retain(|j| j.id != id || j.is_active());
}

/// 用系统默认浏览器打开 URL（URL 含 `&`，必须整体加引号，否则被 cmd 截断）。
pub fn open_external(url: &str) {
    log_line(&format!("open_external: {url}"));
    if open_via_cmd(url).is_err() {
        let _ = Command::new("explorer.exe").arg(url).spawn();
    }
}

fn open_via_cmd(url: &str) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let safe = url.replace('"', "%22");
    Command::new("cmd")
        .raw_arg("/C")
        .raw_arg(format!("start \"\" \"{safe}\""))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    Ok(())
}

/// 诊断日志：%APPDATA%\StardewModManager\watch.log（append）。
pub fn log_line(msg: &str) {
    let _ = (|| -> std::io::Result<()> {
        let path = crate::model::config_dir().join("watch.log");
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

/// 下载文件夹解析结果缓存。
static DOWNLOADS_DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// 系统下载文件夹（注册表读取用户自定义位置，失败回退 %USERPROFILE%\Downloads）。
///
/// 结果**必须缓存**：底层要起一个 `reg query` 子进程（实测 16~28ms），而 UI 每帧
/// 都要显示「监控目录：…」。下载期间界面以 20~30fps 重绘，每帧起子进程等于每秒
/// 堵住 UI 线程几百毫秒，表现就是「下载时卡死、下载完就好」。目录位置运行期不变。
pub fn downloads_dir() -> Option<PathBuf> {
    DOWNLOADS_DIR.get_or_init(resolve_downloads_dir).clone()
}

fn resolve_downloads_dir() -> Option<PathBuf> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const GUID: &str = "{374DE290-123F-4565-9164-39C4925E467B}";
    if let Ok(out) = Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Shell Folders",
            "/v",
            GUID,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if line.contains(GUID)
                && let Some(pos) = line.find("REG_SZ")
            {
                let p = line[pos + "REG_SZ".len()..].trim();
                if !p.is_empty() {
                    return Some(PathBuf::from(p));
                }
            }
        }
    }
    std::env::var("USERPROFILE")
        .ok()
        .map(|h| PathBuf::from(h).join("Downloads"))
}

// ---------- 监控线程 ----------

fn watcher_loop() {
    loop {
        if !SESSION.load(Ordering::SeqCst) {
            // 会话关闭：线程挂起等待下次开启。
            THREAD_SPAWNED.store(false, Ordering::SeqCst);
            loop {
                if SESSION.load(Ordering::SeqCst) {
                    THREAD_SPAWNED.store(true, Ordering::SeqCst);
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
        std::thread::sleep(Duration::from_millis(700));
        scan_once();
        process_queue();
    }
}

fn scan_once() {
    let Some(dir) = downloads_dir() else { return };
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    let now = Instant::now();

    // (路径, 文件名小写)
    let entries: Vec<(PathBuf, String)> = rd
        .filter_map(|e| e.ok())
        .map(|e| {
            (
                e.path(),
                e.file_name().to_string_lossy().to_ascii_lowercase(),
            )
        })
        .collect();

    let mut updates: Vec<(String, Option<PathBuf>, u64)> = Vec::new(); // (final_name, path, done)
    let mut in_progress_finals: HashSet<String> = HashSet::new();

    // 1) 进行中的下载（可推导最终名）。
    for (path, name) in &entries {
        let final_name = if let Some(base) = name.strip_suffix(".crdownload") {
            base.to_string()
        } else if let Some(base) = name.strip_suffix(".part") {
            base.to_string()
        } else {
            continue;
        };
        if !final_name.ends_with(".zip") {
            continue;
        }
        in_progress_finals.insert(final_name.clone());
        let done = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        updates.push((final_name, None, done));
    }

    // 2) 已出现的 zip：等待大小稳定后判定完成。
    let mut completed: Vec<(PathBuf, String)> = Vec::new();
    for (path, name) in &entries {
        if !name.ends_with(".zip") || name.starts_with('.') {
            continue;
        }
        let baseline_hit = store().lock().unwrap().baseline.contains(name);
        if baseline_hit || in_progress_finals.contains(name.as_str()) {
            continue;
        }
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let stable = {
            let mut st = store().lock().unwrap();
            let e = st
                .watch_zip
                .entry(path.clone())
                .or_insert((size, now));
            if e.0 != size {
                *e = (size, now);
                false
            } else {
                now.duration_since(e.1) >= Duration::from_millis(1500)
            }
        };
        if stable {
            completed.push((path.clone(), name.clone()));
        }
    }

    // 应用进度更新。
    {
        let mut st = store().lock().unwrap();
        for (final_name, path, done) in updates {
            match st.jobs.iter_mut().find(|j| j.title == final_name) {
                Some(j) if j.state == WState::Downloading => {
                    j.done = done;
                    if path.is_some() {
                        j.path = path.clone();
                    }
                }
                Some(_) => {}
                None => {
                    let id = st.next_id;
                    st.next_id += 1;
                    st.jobs.push(WatchJob {
                        id,
                        title: final_name.clone(),
                        path: None,
                        state: WState::Downloading,
                        done,
                    });
                    log_line(&format!("发现浏览器下载：{final_name}"));
                }
            }
        }
    }

    // 完成的 zip 入安装队列。
    if !completed.is_empty() {
        let mut st = store().lock().unwrap();
        for (path, name) in completed {
            st.watch_zip.remove(&path);
            let display = dir.join(&name).display().to_string();
            let id = match st.jobs.iter().find(|j| j.title == name) {
                Some(j) => {
                    j.id
                }
                None => {
                    let id = st.next_id;
                    st.next_id += 1;
                    st.jobs.push(WatchJob {
                        id,
                        title: name.clone(),
                        path: None,
                        state: WState::Downloading,
                        done: 0,
                    });
                    log_line(&format!("发现已完成的浏览器下载：{display}"));
                    id
                }
            };
            let job = st.jobs.iter_mut().find(|j| j.id == id).unwrap();
            job.state = WState::Installing;
            job.path = Some(dir.join(&name));
            job.done = std::fs::metadata(dir.join(&name)).map(|m| m.len()).unwrap_or(0);
            if !st.queue.contains(&id) {
                st.queue.push_back(id);
            }
        }
    }
}

fn process_queue() {
    loop {
        let id = {
            let mut st = store().lock().unwrap();
            st.queue.pop_front()
        };
        let Some(id) = id else { return };
        // 兜底 panic：否则监控线程会静默死掉，之后浏览器下载再也不会被接管。
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| install_job(id)));
        if r.is_err() {
            log_line(&format!("安装任务 panic（id={id}），已标记为失败"));
            finish(id, false, "安装任务异常终止，请重试".to_string());
        }
    }
}

fn install_job(id: u64) {
    let (path, title) = {
        let st = store().lock().unwrap();
        match st.jobs.iter().find(|j| j.id == id) {
            Some(j) => (j.path.clone(), j.title.clone()),
            None => return,
        }
    };
    let Some(path) = path else {
        finish(id, false, "找不到下载文件".to_string());
        return;
    };
    log_line(&format!("开始安装：{}", path.display()));

    let mods_dir = MODS_DIR.lock().unwrap().clone();
    let Some(mods_dir) = mods_dir else {
        finish(id, false, "未配置游戏目录（设置页指定后重试）".to_string());
        return;
    };

    // 校验：有效 zip 且包含 manifest.json。
    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) => {
            finish(id, false, format!("无法读取文件：{e}（压缩包已保留）"));
            return;
        }
    };
    let mut arch = match ZipArchive::new(file) {
        Ok(a) => a,
        Err(e) => {
            finish(id, false, format!("不是有效的 zip 文件：{e}（已保留原文件）"));
            return;
        }
    };
    let mut has_manifest = false;
    for i in 0..arch.len() {
        if let Ok(f) = arch.by_index(i) {
            let n = f.name().replace('\\', "/");
            if n == "manifest.json" || n.ends_with("/manifest.json") {
                has_manifest = true;
                break;
            }
        }
    }
    if !has_manifest {
        finish(
            id,
            false,
            "不是 SMAPI 模组压缩包（未找到 manifest.json），已保留原文件".to_string(),
        );
        return;
    }

    // 安装。
    match crate::installer::install_archive(&path, &mods_dir) {
        Ok(names) => {
            let del = std::fs::remove_file(&path);
            let tail = match del {
                Ok(()) => String::new(),
                Err(_) => "（压缩包未能自动清理）".to_string(),
            };
            log_line(&format!("安装成功：{} -> {}", title, names.join("、")));
            finish(
                id,
                true,
                format!("已安装：{}{tail}", names.join("、")),
            );
        }
        Err(e) => {
            log_line(&format!("安装失败：{title}: {e}"));
            finish(id, false, format!("安装失败：{e}（压缩包已保留）"));
        }
    }
}

fn finish(id: u64, ok: bool, detail: String) {
    let mut st = store().lock().unwrap();
    if let Some(j) = st.jobs.iter_mut().find(|j| j.id == id) {
        j.state = WState::Finished(ok, detail);
    }
}
