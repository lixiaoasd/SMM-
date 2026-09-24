//! P2P 联机大厅：登录 / 好友 / 按需拉取好友模组。
//!
//! 基于 vnt 虚拟局域网，无需中央服务器：
//!   - 内嵌 HTTP 服务器（端口 8772，**只读**）：`/profile`、`/mods`、`/mods/<folder>`
//!   - UDP 广播（端口 8773）：发现同网段的在线玩家
//!   - P2P 客户端：拉取好友档案、浏览共享模组、下载缺失模组
//!
//! 「登录」= 创建本地档案（昵称 + 自动生成 UID），「上线」= 启动 P2P 服务 + 广播存在。
//! 「好友」= 本地持久化 UID 列表，在 vnt 同网段发现时自动标记在线。
//!
//! 服务器不提供任何写入接口：同网段任何人都能访问它，一旦允许推送 zip 就等于
//! 允许对方往 Mods 里塞 DLL（模组即代码）。补齐模组一律由**本机主动拉取**。

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const P2P_PORT: u16 = 8772;
pub const DISCOVERY_PORT: u16 = 8773;

static P2P_RUNNING: AtomicBool = AtomicBool::new(false);
static PEERS: OnceLock<Mutex<Vec<PeerInfo>>> = OnceLock::new();

pub const AVATARS: &[&str] = &["🌾", "🐔", "⛏️", "🎣", "🌻", "🧑‍🌾", "🐮", "🦊"];

// ============================================================
// 数据结构
// ============================================================

/// 本地玩家档案（%APPDATA%\StardewModManager\profile.json）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub uid: String,
    pub name: String,
    pub avatar: String,
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            uid: String::new(),
            name: String::new(),
            avatar: "🌾".to_string(),
        }
    }
}

/// UDP 广播的心跳包。
#[derive(Serialize, Deserialize)]
struct Heartbeat {
    uid: String,
    name: String,
    avatar: String,
    port: u16,
}

/// 发现的附近玩家。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub uid: String,
    pub name: String,
    pub ip: String,
    pub port: u16,
    pub avatar: String,
    pub last_seen: u64,
}

impl PeerInfo {
    pub fn is_online(&self) -> bool {
        let now = unix_secs();
        now.saturating_sub(self.last_seen) < 20
    }

    pub fn is_friend(&self, friends: &[Friend]) -> bool {
        friends.iter().any(|f| f.uid == self.uid)
    }
}

/// 本地持久化的好友。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Friend {
    pub uid: String,
    pub name: String,
    pub avatar: String,
    pub note: String,
}

/// 对外共享的模组信息（HTTP `/mods` 返回）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SharedMod {
    pub folder: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub unique_id: String,
    pub size: u64,
}

impl SharedMod {
    pub fn size_label(&self) -> String {
        if self.size == 0 {
            return "-".to_string();
        }
        const KB: u64 = 1024;
        const MB: u64 = 1024 * KB;
        const GB: u64 = 1024 * MB;
        if self.size >= GB {
            format!("{:.2} GB", self.size as f64 / GB as f64)
        } else if self.size >= MB {
            format!("{:.1} MB", self.size as f64 / MB as f64)
        } else if self.size >= KB {
            format!("{} KB", self.size / KB)
        } else {
            format!("{} B", self.size)
        }
    }
}

// ============================================================
// 工具
// ============================================================

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn generate_uid() -> String {
    // 用时间戳低 32 位 + 进程 ID 异或生成 8 位 hex，足够区分。
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u32)
        .unwrap_or(0);
    let pid = std::process::id();
    format!("{:08x}", ms ^ (pid as u32).wrapping_mul(2654435761))
}

fn config_dir() -> PathBuf {
    std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("StardewModManager")
}

// ============================================================
// 档案持久化
// ============================================================

pub fn profile_path() -> PathBuf {
    config_dir().join("profile.json")
}

pub fn load_profile() -> Option<Profile> {
    let text = std::fs::read_to_string(profile_path()).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn save_profile(p: &Profile) -> Result<()> {
    let path = profile_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(p)?)?;
    Ok(())
}

// ============================================================
// 好友持久化
// ============================================================

pub fn friends_path() -> PathBuf {
    config_dir().join("friends.json")
}

pub fn load_friends() -> Vec<Friend> {
    let p = friends_path();
    if let Ok(text) = std::fs::read_to_string(&p) {
        if let Ok(list) = serde_json::from_str(&text) {
            return list;
        }
    }
    Vec::new()
}

pub fn save_friends(f: &[Friend]) -> Result<()> {
    let path = friends_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(f)?)?;
    Ok(())
}

// ============================================================
// 共享模组列表
// ============================================================

/// 扫描 Mods 目录，返回可共享的模组列表（跳过隐藏/禁用的）。
pub fn list_shared_mods(mods_path: &Path) -> Vec<SharedMod> {
    scan_mods(mods_path, false)
}

/// 本地模组清单（**含已禁用**的 `.` 前缀目录），仅用于「好友模组对比」。
///
/// 禁用只代表没启用、不代表没有。若对比时把禁用的模组排除在外，它会被
/// 判成「缺失」并被重新下载安装，甚至在 Mods 里留下「.X 与 X」两份同名模组。
pub fn list_local_mods(mods_path: &Path) -> Vec<SharedMod> {
    scan_mods(mods_path, true)
}

fn scan_mods(mods_path: &Path, include_disabled: bool) -> Vec<SharedMod> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(mods_path) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let raw = entry.file_name().to_string_lossy().to_string();
        let (folder, disabled) = match raw.strip_prefix('.') {
            Some(rest) if !rest.is_empty() => (rest.to_string(), true),
            _ => (raw, false),
        };
        if disabled && !include_disabled {
            continue;
        }
        let (name, version, author, unique_id) =
            match std::fs::read_to_string(path.join("manifest.json")) {
                Ok(text) => match crate::model::Manifest::parse(&text) {
                    Some(m) => (m.name, m.version, m.author, m.unique_id),
                    None => (folder.clone(), String::new(), String::new(), String::new()),
                },
                Err(_) => (folder.clone(), String::new(), String::new(), String::new()),
            };
        out.push(SharedMod {
            folder,
            name,
            version,
            author,
            unique_id,
            size: dir_size(&path),
        });
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            } else if p.is_dir() {
                total += dir_size(&p);
            }
        }
    }
    total
}

// ============================================================
// Zip 工具
// ============================================================

/// 把文件夹打包成 zip（内存中）。
fn zip_folder(src: &Path) -> Result<Vec<u8>> {
    let cursor = std::io::Cursor::new(Vec::<u8>::new());
    let mut zw = zip::ZipWriter::new(cursor);
    let opts = zip::write::FileOptions::default();

    for entry in walkdir::WalkDir::new(src) {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            let rel = path.strip_prefix(src)?;
            let name = rel
                .to_str()
                .ok_or_else(|| anyhow!("路径含非 UTF-8 字符"))?
                .replace('\\', "/");
            zw.start_file(&name, opts)?;
            let mut f = std::fs::File::open(path)?;
            std::io::copy(&mut f, &mut zw)?;
        }
    }
    let cursor = zw.finish()?;
    Ok(cursor.into_inner())
}

fn sanitize_join(base: &Path, rel: &str) -> Result<PathBuf> {
    let p = Path::new(rel);
    if p.is_absolute()
        || p
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(anyhow!("不安全的路径：{rel}"));
    }
    Ok(base.join(p))
}

// ============================================================
// HTTP 服务器
// ============================================================

pub fn is_running() -> bool {
    P2P_RUNNING.load(Ordering::SeqCst)
}

/// 启动 P2P 服务（HTTP + UDP 发现）。返回 Err 表示端口绑定失败。
pub fn start_server(mods_path: PathBuf, profile: Profile) -> Result<(), String> {
    if P2P_RUNNING.load(Ordering::SeqCst) {
        return Err("P2P 服务已在运行".to_string());
    }

    let listener = TcpListener::bind(("0.0.0.0", P2P_PORT))
        .map_err(|e| format!("无法绑定端口 {P2P_PORT}：{e}（是否已开服？）"))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("设置非阻塞失败：{e}"))?;

    P2P_RUNNING.store(true, Ordering::SeqCst);

    // HTTP 服务线程
    let p2 = mods_path.clone();
    let pr = profile.clone();
    std::thread::spawn(move || {
        while P2P_RUNNING.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let p2 = p2.clone();
                    let pr = pr.clone();
                    std::thread::spawn(move || {
                        let _ = handle_http(stream, &p2, &pr);
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(_) => break,
            }
        }
    });

    // UDP 发现线程
    std::thread::spawn(move || {
        discovery_loop(profile);
    });

    Ok(())
}

pub fn stop_server() {
    P2P_RUNNING.store(false, Ordering::SeqCst);
    if let Some(peers) = PEERS.get() {
        peers.lock().unwrap().clear();
    }
}

/// 请求行 / 单个请求头的长度上限：对端一直不发换行也不会把内存撑爆。
const MAX_LINE: usize = 8 * 1024;

/// 带上限地读一行；超限直接报错断开连接。
fn read_line_limited(reader: &mut impl BufRead, out: &mut String) -> std::io::Result<()> {
    let n = reader.by_ref().take(MAX_LINE as u64).read_line(out)?;
    if n >= MAX_LINE && !out.ends_with('\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "请求行/请求头过长",
        ));
    }
    Ok(())
}

/// 写一个完整响应（HTTP/1.0 + CORS 头）。
fn respond(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let header = format!(
        "HTTP/1.0 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
         Access-Control-Allow-Headers: *\r\n\
         \r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    Ok(())
}

fn handle_http(mut stream: TcpStream, mods_path: &Path, profile: &Profile) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(60)))?;

    let mut reader = BufReader::new(stream.try_clone()?);

    // 读请求行
    let mut request_line = String::new();
    read_line_limited(&mut reader, &mut request_line)?;
    let parts: Vec<&str> = request_line.trim().split_whitespace().collect();
    let method = parts.first().copied().unwrap_or("");
    let path = parts.get(1).copied().unwrap_or("/");

    // 读 headers（只用于跳过，本服务没有 POST 路由，不解析 content-length；
    // 请求体也一律不读——留着它就会成为「声明超大 Content-Length 打爆内存」的口子）。
    loop {
        let mut line = String::new();
        read_line_limited(&mut reader, &mut line)?;
        if line.trim().is_empty() {
            break;
        }
    }

    // 路由（只读接口：档案 / 模组清单 / 单个模组 zip）。
    let (status, content_type, response_body): (&str, &str, Vec<u8>) = match (method, path) {
        ("GET", "/") | ("GET", "/profile") => {
            let json = serde_json::to_string(profile).unwrap_or_default();
            ("200 OK", "application/json", json.into_bytes())
        }
        ("GET", "/mods") => {
            let mods = list_shared_mods(mods_path);
            let json = serde_json::to_string(&mods).unwrap_or_default();
            ("200 OK", "application/json", json.into_bytes())
        }
        ("GET", p) if p.starts_with("/mods/") => {
            let folder = url_decode(&p[6..]);
            // 用 sanitize_join 防止路径穿越（拒绝 .. 和绝对路径）。
            match sanitize_join(mods_path, &folder) {
                Ok(mod_path) if mod_path.is_dir() => {
                    match zip_folder(&mod_path) {
                        Ok(data) => ("200 OK", "application/zip", data),
                        Err(_) => (
                            "500 Internal Server Error",
                            "text/plain",
                            b"Zip failed".to_vec(),
                        ),
                    }
                }
                _ => ("404 Not Found", "text/plain", b"Not found".to_vec()),
            }
        }
        ("OPTIONS", _) => {
            // CORS preflight
            (
                "200 OK",
                "text/plain",
                Vec::new(),
            )
        }
        _ => ("404 Not Found", "text/plain", b"Not found".to_vec()),
    };

    // 发响应
    respond(&mut stream, status, content_type, &response_body)?;

    Ok(())
}

// ============================================================
// UDP 发现
// ============================================================

fn discovery_loop(profile: Profile) {
    let listener = match UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT)) {
        Ok(s) => s,
        Err(_) => return,
    };
    listener
        .set_read_timeout(Some(Duration::from_millis(500)))
        .ok();

    let broadcaster = match UdpSocket::bind(("0.0.0.0", 0)) {
        Ok(s) => s,
        Err(_) => return,
    };
    broadcaster.set_broadcast(true).ok();

    let heartbeat = serde_json::to_string(&Heartbeat {
        uid: profile.uid.clone(),
        name: profile.display_name(),
        avatar: profile.avatar.clone(),
        port: P2P_PORT,
    })
    .unwrap_or_default();
    let hb_bytes = heartbeat.as_bytes();

    let mut last_broadcast = Instant::now();

    while P2P_RUNNING.load(Ordering::SeqCst) {
        // 每 5 秒广播一次
        if last_broadcast.elapsed() > Duration::from_secs(5) {
            let _ = broadcaster.send_to(hb_bytes, ("255.255.255.255", DISCOVERY_PORT));
            last_broadcast = Instant::now();
        }

        // 监听其他玩家的心跳
        let mut buf = [0u8; 1024];
        match listener.recv_from(&mut buf) {
            Ok((n, src)) => {
                if let Ok(json) = std::str::from_utf8(&buf[..n]) {
                    if let Ok(hb) = serde_json::from_str::<Heartbeat>(json) {
                        // 不把自己加进去
                        if hb.uid != profile.uid {
                            let ip = src.ip().to_string();
                            let now = unix_secs();
                            let peer = PeerInfo {
                                uid: hb.uid,
                                name: hb.name,
                                ip,
                                port: hb.port,
                                avatar: hb.avatar,
                                last_seen: now,
                            };
                            add_peer(peer);
                        }
                    }
                }
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => {}
        }
    }
}

fn add_peer(peer: PeerInfo) {
    let peers = PEERS.get_or_init(|| Mutex::new(Vec::new()));
    let mut list = peers.lock().unwrap();
    if let Some(existing) = list.iter_mut().find(|p| p.uid == peer.uid) {
        *existing = peer;
    } else {
        list.push(peer);
    }
}

/// 返回当前发现的在线玩家列表（过滤掉超时的）。
pub fn get_peers() -> Vec<PeerInfo> {
    let now = unix_secs();
    PEERS.get()
        .map(|m| {
            m.lock()
                .unwrap()
                .iter()
                .filter(|p| now.saturating_sub(p.last_seen) < 20)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

// ============================================================
// P2P 客户端
// ============================================================

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new())
}

/// 拉取好友的共享模组列表。
pub fn fetch_peer_mods(ip: &str) -> Result<Vec<SharedMod>> {
    let url = format!("http://{ip}:{P2P_PORT}/mods");
    let resp = client().get(&url).send()?;
    if resp.status().is_success() {
        Ok(resp.json()?)
    } else {
        Err(anyhow!("HTTP {}", resp.status()))
    }
}

/// 从好友处下载模组并自动安装到 Mods 目录。
pub fn download_mod_from_peer(ip: &str, folder: &str, mods_path: &Path) -> Result<String> {
    let data = fetch_mod_bytes_from_peer(ip, folder)?;
    // 统一走 installer：带全局安装锁，不会与镜像直装/浏览器监控并发写 Mods。
    crate::installer::install_zip_bytes(&data, mods_path)?;
    Ok(format!("「{folder}」已从 {ip} 下载并安装"))
}

/// 从好友处取回模组 zip 字节（只读，不落盘、不安装）。
///
/// 单独拆出来是给「好友 + 镜像竞速」用的：两条线程只负责取字节，
/// 由调用方在拿到第一个成功结果后安装一次，避免两边各解压一遍互相覆盖。
fn fetch_mod_bytes_from_peer(ip: &str, folder: &str) -> Result<Vec<u8>> {
    let url = format!("http://{ip}:{P2P_PORT}/mods/{folder}");
    let resp = client().get(&url).send()?;
    if !resp.status().is_success() {
        return Err(anyhow!("HTTP {}", resp.status()));
    }
    Ok(resp.bytes()?.to_vec())
}

// ============================================================
// 模组同步：自动侦测好友模组 → 补齐缺失
// ============================================================

/// 下载来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadSource {
    /// 好友 + 服务器同时下载，谁先完成用谁。
    Both,
    /// 仅从镜像服务器下载。
    Mirror,
    /// 仅从好友 P2P 下载。
    Friend,
}

/// 模组对比结果。
#[derive(Debug, Clone, Default)]
pub struct ModSync {
    /// 好友有但本地缺少的模组。
    pub missing: Vec<SharedMod>,
    /// 双方都有的模组。
    pub matching: Vec<SharedMod>,
    /// 本地有但好友没有的模组。
    pub extra: Vec<SharedMod>,
}

/// 对比用的模组身份键：优先 UniqueID（同一模组改名/改文件夹都不受影响），
/// 没有 manifest 的退回文件夹名。
fn mod_key(m: &SharedMod) -> String {
    let uid = m.unique_id.trim();
    if uid.is_empty() {
        m.folder.trim_start_matches('.').to_lowercase()
    } else {
        uid.to_uppercase()
    }
}

/// 对比本地与好友的模组列表，找出缺失的。
pub fn compare_mods(local: &[SharedMod], peer: &[SharedMod]) -> ModSync {
    let local_set: std::collections::HashSet<String> = local.iter().map(mod_key).collect();
    let peer_set: std::collections::HashSet<String> = peer.iter().map(mod_key).collect();

    let missing = peer
        .iter()
        .filter(|m| !local_set.contains(&mod_key(m)))
        .cloned()
        .collect();
    let matching = peer
        .iter()
        .filter(|m| local_set.contains(&mod_key(m)))
        .cloned()
        .collect();
    let extra = local
        .iter()
        .filter(|m| !peer_set.contains(&mod_key(m)))
        .cloned()
        .collect();

    ModSync { missing, matching, extra }
}

/// 镜像服务器上的模组条目（仅取需要的字段）。
#[derive(Deserialize)]
struct MirrorEntry {
    name: String,
    file: String,
}

/// 从镜像服务器取回模组 zip 字节（只读，不落盘、不安装）。
fn fetch_mod_bytes_from_mirror(
    folder: &str,
    mirror_url: &str,
) -> Result<(String, Vec<u8>)> {
    let base = mirror_url.trim_end_matches('/');
    let index_url = format!("{base}/index.json");
    let resp = client().get(&index_url).send()?;
    if !resp.status().is_success() {
        return Err(anyhow!("镜像服务器不可用（HTTP {}）", resp.status()));
    }
    let entries: Vec<MirrorEntry> = resp.json()?;
    // 按模组名模糊匹配（忽略大小写）
    let found = entries
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case(folder));
    let Some(m) = found else {
        return Err(anyhow!("镜像服务器上找不到「{folder}」"));
    };
    let file_url = format!("{base}/{}", m.file);
    let resp2 = client().get(&file_url).send()?;
    if !resp2.status().is_success() {
        return Err(anyhow!("下载失败（HTTP {}）", resp2.status()));
    }
    Ok((m.name.clone(), resp2.bytes()?.to_vec()))
}

/// 从镜像服务器下载模组并安装。
pub fn download_from_mirror(
    folder: &str,
    mirror_url: &str,
    mods_path: &Path,
) -> Result<String> {
    let (name, data) = fetch_mod_bytes_from_mirror(folder, mirror_url)?;
    crate::installer::install_zip_bytes(&data, mods_path)?;
    Ok(format!("「{name}」已从镜像服务器下载并安装"))
}

/// 同时从好友和镜像服务器下载，谁先拿到完整数据用谁。
///
/// 两条线程**只负责取字节**，安装由本函数在收到第一个成功结果后做**一次**：
/// 原实现让两条线程各自解压到 Mods，慢的一方不会停手，会把两个版本的文件
/// 混在一起（同名后写赢、独有文件两边都留下），且绕过了全局安装锁。
fn download_from_both(
    ip: &str,
    folder: &str,
    mirror_url: &str,
    mods_path: &Path,
) -> Result<String> {
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel::<(&'static str, Result<Vec<u8>>)>();

    // 线程 1：好友
    let tx1 = tx.clone();
    let ip1 = ip.to_string();
    let folder1 = folder.to_string();
    std::thread::spawn(move || {
        let r = fetch_mod_bytes_from_peer(&ip1, &folder1);
        let _ = tx1.send(("friend", r));
    });

    // 线程 2：镜像服务器
    let tx2 = tx.clone();
    let folder2 = folder.to_string();
    let url2 = mirror_url.to_string();
    std::thread::spawn(move || {
        let r = fetch_mod_bytes_from_mirror(&folder2, &url2).map(|(_, data)| data);
        let _ = tx2.send(("mirror", r));
    });

    drop(tx); // 关闭发送端：两个线程都结束后 recv 会返回 Disconnected

    let deadline = Instant::now() + Duration::from_secs(180);
    let mut last_err = String::new();
    loop {
        let remain = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remain) {
            Ok((source, Ok(data))) => {
                // 安装失败（拿到的是坏包）时继续等另一个来源，别把这次补齐直接判死。
                match crate::installer::install_zip_bytes(&data, mods_path) {
                    Ok(_) => return Ok(format!("[{source}] 「{folder}」已下载并安装")),
                    Err(e) => last_err = format!("{source} 安装失败：{e}"),
                }
            }
            Ok((_, Err(e))) => last_err = e.to_string(),
            Err(mpsc::RecvTimeoutError::Timeout) => return Err(anyhow!("下载超时")),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(anyhow!("两个下载源都失败了。{last_err}"))
            }
        }
    }
}

/// 根据指定来源下载单个模组。
pub fn download_mod(
    source: DownloadSource,
    ip: &str,
    folder: &str,
    mirror_url: &str,
    mods_path: &Path,
) -> Result<String> {
    match source {
        DownloadSource::Friend => download_mod_from_peer(ip, folder, mods_path),
        DownloadSource::Mirror => download_from_mirror(folder, mirror_url, mods_path),
        DownloadSource::Both => download_from_both(ip, folder, mirror_url, mods_path),
    }
}

/// 批量补齐缺失模组。
///
/// `progress` 回调在每完成一个模组时被调用（index 从 0 开始，total = missing.len()）。
pub fn fill_missing(
    ip: &str,
    missing: &[SharedMod],
    source: DownloadSource,
    mirror_url: &str,
    mods_path: &Path,
    progress: &dyn Fn(usize, usize, &str),
) -> (usize, usize) {
    let total = missing.len();
    let mut ok = 0usize;
    let mut fail = 0usize;
    for (i, m) in missing.iter().enumerate() {
        let r = download_mod(source, ip, &m.folder, mirror_url, mods_path);
        match &r {
            Ok(msg) => {
                ok += 1;
                progress(i + 1, total, msg);
            }
            Err(e) => {
                fail += 1;
                progress(i + 1, total, &format!("「{}」失败：{e}", m.folder));
            }
        }
    }
    (ok, fail)
}

// ============================================================
// Helpers
// ============================================================

impl Profile {
    pub fn display_name(&self) -> String {
        if self.name.is_empty() {
            "匿名玩家".to_string()
        } else {
            self.name.clone()
        }
    }
}

fn url_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let h1 = chars.next();
            let h2 = chars.next();
            if let (Some(h1), Some(h2)) = (h1, h2) {
                if let Ok(byte) = u8::from_str_radix(&format!("{h1}{h2}"), 16) {
                    out.push(byte as char);
                    continue;
                }
            }
            out.push('%');
        } else if c == '+' {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}
