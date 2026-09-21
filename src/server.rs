//! 一键开服 / 房主助手 / 内网穿透（vnt）业务逻辑。
//!
//! 与游戏内插件 `FireSVM.HostKit` 通过 `Mods\FireSVM.HostKit` 下的三个文件通信：
//!   - `hostkit-config.json`  管理器 → 插件：开服参数
//!   - `hostkit-status.json`  插件 → 管理器：房主实时状态
//!   - `hostkit-cmd.txt`      管理器 → 插件：一次性指令（freeze / unfreeze / announce / quit）

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// 子进程不弹控制台窗口。
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 星露谷联机使用的 UDP 端口。
pub const GAME_PORT: u16 = 24642;

// ============================================================
// 内嵌的房主助手插件（编译期打进 exe，开服时释放到 Mods）
// ============================================================

static HOSTKIT_DLL: &[u8] = include_bytes!("../stardew-hostkit/bin/FireSVM.HostKit.dll");
static HOSTKIT_MANIFEST: &[u8] = include_bytes!("../stardew-hostkit/manifest.json");

const HOSTKIT_FOLDER: &str = "FireSVM.HostKit";
const CONFIG_FILE: &str = "hostkit-config.json";
const STATUS_FILE: &str = "hostkit-status.json";
const CMD_FILE: &str = "hostkit-cmd.txt";

/// 插件目录绝对路径。
pub fn hostkit_dir(game: &Path) -> PathBuf {
    game.join("Mods").join(HOSTKIT_FOLDER)
}

/// 释放（或更新）内嵌插件到 Mods 目录；内容一致时不重复写盘。
pub fn deploy_hostkit(game: &Path) -> Result<()> {
    let dir = hostkit_dir(game);
    std::fs::create_dir_all(&dir)?;
    let dll = dir.join("FireSVM.HostKit.dll");
    if std::fs::read(&dll).map(|b| b != HOSTKIT_DLL).unwrap_or(true) {
        std::fs::write(&dll, HOSTKIT_DLL)?;
    }
    let mf = dir.join("manifest.json");
    if std::fs::read(&mf).map(|b| b != HOSTKIT_MANIFEST).unwrap_or(true) {
        std::fs::write(&mf, HOSTKIT_MANIFEST)?;
    }
    // 指令文件必须存在，否则插件每秒的 File.Exists 判断会直接跳过。
    let cmd = dir.join(CMD_FILE);
    if !cmd.exists() {
        std::fs::write(&cmd, "")?;
    }
    Ok(())
}

// ============================================================
// 插件配置 / 状态（字段名与 C# 侧保持一致，用 PascalCase）
// ============================================================

/// 房主助手配置：既写入 `hostkit-config.json`，也持久化在管理器设置里。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HostKitConfig {
    /// 启动游戏时自动读取 SaveName 存档并开房；插件消费一次后自行置回 false。
    #[serde(rename = "AutoHost")]
    pub auto_host: bool,
    /// 要开的存档文件夹名（`%APPDATA%\StardewValley\Saves` 下的目录名）。
    #[serde(rename = "SaveName")]
    pub save_name: String,
    /// 房主无操作达到 afk_minutes 分钟时自动暂停游戏时间。
    #[serde(rename = "AfkPause")]
    pub afk_pause: bool,
    #[serde(rename = "AfkMinutes")]
    pub afk_minutes: i32,
    /// 没有其他玩家在线达到 empty_minutes 分钟时自动暂停（等人时农场不空跑）。
    #[serde(rename = "EmptyPause")]
    pub empty_pause: bool,
    #[serde(rename = "EmptyMinutes")]
    pub empty_minutes: i32,
    /// 玩家上下线公告。
    #[serde(rename = "AnnounceJoin")]
    pub announce_join: bool,
    #[serde(rename = "AnnounceLeave")]
    pub announce_leave: bool,
    /// 新玩家加入时的欢迎语（空串表示不发送）。
    #[serde(rename = "WelcomeText")]
    pub welcome_text: String,
    /// 手动冻结/恢复时间的热键（SButton 名，如 F8）。
    #[serde(rename = "FreezeHotkey")]
    pub freeze_hotkey: String,
    /// 暂停/恢复时在游戏内聊天框提示。
    #[serde(rename = "NotifyPause")]
    pub notify_pause: bool,
}

impl Default for HostKitConfig {
    fn default() -> Self {
        HostKitConfig {
            auto_host: false,
            save_name: String::new(),
            afk_pause: true,
            afk_minutes: 5,
            empty_pause: false,
            empty_minutes: 10,
            announce_join: true,
            announce_leave: true,
            welcome_text: String::new(),
            freeze_hotkey: "F8".to_string(),
            notify_pause: true,
        }
    }
}

/// 插件写出的房主实时状态。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HostStatus {
    #[serde(rename = "Hosting")]
    pub hosting: bool,
    #[serde(rename = "TimePaused")]
    pub time_paused: bool,
    /// 暂停原因：afk / empty / manual；未暂停为空串。
    #[serde(rename = "PausedBy")]
    pub paused_by: String,
    #[serde(rename = "SaveName")]
    pub save_name: String,
    #[serde(rename = "PlayerName")]
    pub player_name: String,
    #[serde(rename = "FarmName")]
    pub farm_name: String,
    #[serde(rename = "Players")]
    pub players: i32,
    #[serde(rename = "PlayerNames")]
    pub player_names: Vec<String>,
    #[serde(rename = "TimeOfDay")]
    pub time_of_day: i32,
    #[serde(rename = "SeasonIndex")]
    pub season_index: i32,
    #[serde(rename = "DayOfMonth")]
    pub day_of_month: i32,
    #[serde(rename = "Year")]
    pub year: i32,
    #[serde(rename = "AfkSeconds")]
    pub afk_seconds: i32,
    #[serde(rename = "EmptySeconds")]
    pub empty_seconds: i32,
    #[serde(rename = "Note")]
    pub note: String,
    #[serde(rename = "LastEventTime")]
    pub last_event_time: String,
}

impl HostStatus {
    /// 游戏内时间，形如 `10:20`。
    pub fn clock(&self) -> String {
        format!("{:02}:{:02}", (self.time_of_day / 100) % 24, self.time_of_day % 100)
    }
    pub fn season_label(&self) -> &'static str {
        match self.season_index {
            0 => "春",
            1 => "夏",
            2 => "秋",
            3 => "冬",
            _ => "?",
        }
    }
    pub fn pause_reason(&self) -> &'static str {
        match self.paused_by.as_str() {
            "afk" => "房主挂机",
            "empty" => "无人在线",
            "manual" => "手动冻结",
            _ => "",
        }
    }
}

/// 状态快照：附带状态文件的写入时间，用于判断是否已过期（游戏没在跑时是旧数据）。
pub struct StatusSnapshot {
    pub status: HostStatus,
    pub age: Duration,
}

/// 写入插件配置（含 AutoHost 开关）。
pub fn write_config(game: &Path, cfg: &HostKitConfig) -> Result<()> {
    let dir = hostkit_dir(game);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(CONFIG_FILE), serde_json::to_string_pretty(cfg)?)?;
    Ok(())
}

/// 读取当前状态；文件不存在或过期（超过 15 秒未刷新）时返回 None。
pub fn read_status(game: &Path) -> Option<StatusSnapshot> {
    let p = hostkit_dir(game).join(STATUS_FILE);
    let meta = std::fs::metadata(&p).ok()?;
    let modified = meta.modified().ok()?;
    let age = SystemTime::now().duration_since(modified).unwrap_or_default();
    let text = std::fs::read_to_string(&p).ok()?;
    let status: HostStatus = serde_json::from_str(&text).ok()?;
    if age > Duration::from_secs(15) {
        return None;
    }
    Some(StatusSnapshot { status, age })
}

/// 给插件下一条指令。指令名：freeze / unfreeze / quit，或 `announce|文本`。
pub fn send_command(game: &Path, cmd: &str) -> Result<()> {
    let dir = hostkit_dir(game);
    if !dir.is_dir() {
        return Err(anyhow!("尚未部署房主助手插件"));
    }
    std::fs::write(dir.join(CMD_FILE), cmd)?;
    Ok(())
}

// ============================================================
// 存档扫描
// ============================================================

pub fn saves_dir() -> PathBuf {
    crate::model::appdata().join("StardewValley").join("Saves")
}

/// 一个可开房的存档。
#[derive(Debug, Clone, Default)]
pub struct SaveInfo {
    /// 存档文件夹名（即 SaveGame.Load 需要的名字）。
    pub folder: String,
    pub player_name: String,
    pub farm_name: String,
    pub day: u32,
    pub season: u32,
    pub year: u32,
    pub play_hours: f64,
    /// 存档是否允许开房（游戏里勾选过「多人」才会是 true）。
    pub can_host: bool,
    /// 距上次保存的秒数。
    pub saved_ago_secs: Option<u64>,
}

impl SaveInfo {
    pub fn season_label(&self) -> &'static str {
        match self.season {
            0 => "春",
            1 => "夏",
            2 => "秋",
            3 => "冬",
            _ => "?",
        }
    }

    /// 「第 1 年 春 6 日」。
    pub fn date_label(&self) -> String {
        format!("第 {} 年 {} {} 日", self.year, self.season_label(), self.day)
    }

    pub fn title(&self) -> String {
        if self.farm_name.trim().is_empty() {
            self.folder.clone()
        } else {
            format!("{} · {}", self.farm_name, self.player_name)
        }
    }

    pub fn saved_ago_label(&self) -> String {
        match self.saved_ago_secs {
            Some(s) if s < 3600 => format!("{} 分钟前", s / 60),
            Some(s) if s < 86400 => format!("{} 小时前", s / 3600),
            Some(s) => format!("{} 天前", s / 86400),
            None => "-".to_string(),
        }
    }
}

/// 取 XML 里第一个 `<tag>…</tag>` 的文本。
fn xml_text(hay: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = hay.find(&open)? + open.len();
    let end = hay[start..].find(&close)? + start;
    Some(hay[start..end].trim().to_string())
}

/// 扫描存档目录，按最近保存时间倒序返回可开房的存档。
pub fn scan_saves() -> Vec<SaveInfo> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(saves_dir()) else {
        return out;
    };
    for e in entries.flatten() {
        let dir = e.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(name) = dir.file_name() else { continue };
        let folder = name.to_string_lossy().to_string();
        if folder.starts_with('.') {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(dir.join("SaveGameInfo")) else {
            continue;
        };
        // 根节点是 <Farmer>，第一个 <name> 就是本人（背包物品的 <name> 都在其后）；
        // farmName / slotCanHost 等字段排在文件靠后位置（约 30KB 处），必须整份搜索。
        let num = |tag: &str| xml_text(&text, tag).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
        let ms = xml_text(&text, "millisecondsPlayed")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        out.push(SaveInfo {
            folder,
            player_name: xml_text(&text, "name").unwrap_or_default(),
            farm_name: xml_text(&text, "farmName").unwrap_or_default(),
            day: num("dayOfMonthForSaveGame"),
            season: num("seasonForSaveGame"),
            year: num("yearForSaveGame"),
            play_hours: ms as f64 / 3_600_000.0,
            can_host: xml_text(&text, "slotCanHost")
                .map(|v| v.eq_ignore_ascii_case("true"))
                .unwrap_or(true),
            saved_ago_secs: std::fs::metadata(dir.join("SaveGameInfo"))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .map(|d| d.as_secs()),
        });
    }
    out.sort_by_key(|s| s.saved_ago_secs.unwrap_or(u64::MAX));
    out
}

// ============================================================
// 进程 / 端口 / 网络
// ============================================================

/// 游戏是否正在运行（SMAPI 或原版）。
pub fn game_running() -> bool {
    for image in ["StardewModdingAPI.exe", "Stardew Valley.exe"] {
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("IMAGENAME eq {image}"), "/NH"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        if let Ok(o) = out
            && String::from_utf8_lossy(&o.stdout).to_lowercase().contains(&image.to_lowercase())
        {
            return true;
        }
    }
    false
}

/// 用 SMAPI 启动游戏。
pub fn launch_smapi(game: &Path) -> Result<()> {
    let exe = game.join("StardewModdingAPI.exe");
    if !exe.is_file() {
        return Err(anyhow!("未找到 StardewModdingAPI.exe，请先在设置页安装 SMAPI"));
    }
    std::process::Command::new(&exe)
        .current_dir(game)
        .spawn()?;
    Ok(())
}

/// 联机端口是否已被占用（UDP 24642 已被监听即视为已被占用）。
pub fn port_in_use(port: u16) -> bool {
    std::net::UdpSocket::bind(("0.0.0.0", port)).is_err()
}

/// 本机局域网 IP（不做真实连接，仅取路由表选出的出口地址）。
pub fn primary_local_ip() -> Option<String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?;
    s.local_addr().ok().map(|a| a.ip().to_string())
}

fn firewall_rule_name(port: u16) -> String {
    format!("Stardew Valley Coop UDP {port}")
}

/// 防火墙是否已放行联机端口。
pub fn firewall_allowed(port: u16) -> bool {
    std::process::Command::new("netsh")
        .args([
            "advfirewall",
            "firewall",
            "show",
            "rule",
            &format!("name={}", firewall_rule_name(port)),
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 以管理员身份放行联机端口（会弹一次 UAC）。
pub fn allow_firewall(port: u16) -> Result<()> {
    let script = format!(
        "$n = '/c'; $c = 'netsh advfirewall firewall add rule name=\"{}\" dir=in action=allow protocol=UDP localport={}'; \
         Start-Process -FilePath cmd.exe -ArgumentList @($n,$c) -Verb RunAs -Wait -ErrorAction Stop",
        firewall_rule_name(port),
        port
    );
    powershell(&script)?;
    Ok(())
}

/// 执行一段 PowerShell 脚本（隐藏窗口），失败时返回 stderr。
fn powershell(script: &str) -> Result<String> {
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    if !out.status.success() {
        return Err(anyhow!(
            "{}",
            String::from_utf8_lossy(&out.stderr).trim().to_string()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// PowerShell 单引号字符串转义。
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// 以管理员身份启动一个程序（UAC 被拒时给出可读错误）。
fn run_elevated(exe: &str, args: &[String]) -> Result<()> {
    let list = args.iter().map(|a| ps_quote(a)).collect::<Vec<_>>().join(",");
    let file = ps_quote(exe);
    // -WindowStyle 与 -Verb RunAs 在部分系统上组合会报参数错误，失败则退回不带窗口样式。
    let script = format!(
        "$a = @({list}); \
         try {{ Start-Process -FilePath {file} -ArgumentList $a -Verb RunAs -WindowStyle Hidden -ErrorAction Stop }} \
         catch {{ Start-Process -FilePath {file} -ArgumentList $a -Verb RunAs -ErrorAction Stop }}"
    );
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let err = String::from_utf8_lossy(&out.stdout).trim().to_string() + &err;
        return Err(anyhow!(
            "{}",
            if err.is_empty() {
                "已取消管理员授权".to_string()
            } else {
                err
            }
        ));
    }
    Ok(())
}

// ============================================================
// vnt 内网穿透（免公网 IP、免端口映射的异地组网）
// ============================================================

/// vnt 运行参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VntOptions {
    /// 组网编号：填相同编号的设备会被组进同一个虚拟局域网。
    pub network_code: String,
    /// 组网密码（可选，同一网络内必须一致）。
    pub password: String,
    /// 服务器地址，可带 `quic://` 前缀，不写则默认 quic。
    pub server: String,
    /// 设备名（留空用本机主机名，方便在虚拟网里认出是谁）。
    pub device_name: String,
    /// 关闭虚拟网内的广播转发（关掉会让「加入局域网游戏」的列表搜不到主机，默认不关）。
    pub no_broadcast: bool,
    /// QUIC 优化传输。
    pub rtx: bool,
    /// FEC 前向纠错：牺牲部分带宽换取链路稳定性。
    pub fec: bool,
}

impl Default for VntOptions {
    fn default() -> Self {
        VntOptions {
            network_code: String::new(),
            password: String::new(),
            // 官方文档给公共服务端；实测本机可连通并拿到 10.1.0.2。
            server: "101.35.230.139:6660".to_string(),
            device_name: String::new(),
            no_broadcast: false,
            rtx: true,
            fec: false,
        }
    }
}

/// 运行期探测结果。
#[derive(Debug, Clone, Default)]
pub struct VntRuntime {
    pub running: bool,
    /// 虚拟网卡上的 IP，例如 10.26.0.3。
    pub ip: Option<String>,
    pub iface: Option<String>,
}

pub fn vnt_dir() -> PathBuf {
    crate::model::config_dir().join("vnt")
}

/// vnt 主程序路径（解压后目录里带 cli 的那个 exe）。
pub fn vnt_exe() -> Option<PathBuf> {
    let mut fallback: Option<PathBuf> = None;
    for e in std::fs::read_dir(vnt_dir()).ok()?.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let name = p.file_name().map(|s| s.to_string_lossy().to_lowercase());
        let Some(name) = name else { continue };
        if !name.ends_with(".exe") {
            continue;
        }
        if name.contains("cli") {
            return Some(p);
        }
        if fallback.is_none() {
            fallback = Some(p);
        }
    }
    fallback
}

pub fn vnt_installed() -> bool {
    vnt_exe().is_some()
}

/// 查询 vnt 最新版 Windows x64 安装包地址。
fn vnt_latest_url() -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("StardewModManager")
        .timeout(Duration::from_secs(30))
        .build()?;
    let json: serde_json::Value = client
        .get("https://api.github.com/repos/vnt-dev/vnt/releases/latest")
        .send()?
        .error_for_status()?
        .json()?;
    let assets = json["assets"]
        .as_array()
        .ok_or_else(|| anyhow!("发布信息结构异常"))?;
    for a in assets {
        let name = a["name"].as_str().unwrap_or("");
        if name.starts_with("vnt2-x86_64-pc-windows-msvc") && name.ends_with(".zip") {
            let url = a["browser_download_url"].as_str().unwrap_or("");
            if !url.is_empty() {
                return Ok(url.to_string());
            }
        }
    }
    Err(anyhow!("未找到 Windows x64 安装包"))
}

/// 下载并解压 vnt，返回主程序路径。
pub fn vnt_download() -> Result<PathBuf> {
    let url = vnt_latest_url()?;
    let client = reqwest::blocking::Client::builder()
        .user_agent("StardewModManager")
        .timeout(Duration::from_secs(600))
        .build()?;
    let bytes = client
        .get(&url)
        .send()?
        .error_for_status()?
        .bytes()?;
    let dir = vnt_dir();
    std::fs::create_dir_all(&dir)?;
    let zip_path = dir.join("vnt-download.zip");
    std::fs::write(&zip_path, &bytes)?;
    extract_zip(&zip_path, &dir)?;
    let _ = std::fs::remove_file(&zip_path);
    vnt_exe().ok_or_else(|| anyhow!("解压完成但未找到 vnt 主程序"))
}

fn extract_zip(zip_path: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    std::fs::create_dir_all(dest)?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry.name().to_string();
        if name.ends_with('/') {
            continue;
        }
        let out = sanitize_join(dest, &name)?;
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::File::create(&out)?;
        std::io::copy(&mut entry, &mut f)?;
    }
    Ok(())
}

fn sanitize_join(base: &Path, rel: &str) -> Result<PathBuf> {
    let mut out = base.to_path_buf();
    for part in rel.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return Err(anyhow!("压缩包内路径非法：{rel}"));
        }
        out.push(part);
    }
    Ok(out)
}

/// 规范化服务器地址：留空时回落到默认公共服务端。
/// vnt2 支持 `host:port` 简写（自动按 TLS/TCP 连接），也可写 `quic://`、`tcp://`、`wss://` 前缀。
/// 注意不要给简写补前缀：`quic://host:port` 与 `host:port` 连的是不同协议，补错会连不上。
pub fn normalize_server(server: &str) -> String {
    let s = server.trim();
    if s.is_empty() {
        VntOptions::default().server
    } else {
        s.to_string()
    }
}

fn toml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// 生成 vnt 配置文件，返回其路径。
fn write_vnt_conf(opt: &VntOptions) -> Result<PathBuf> {
    let dir = vnt_dir();
    std::fs::create_dir_all(&dir)?;
    let server = normalize_server(&opt.server);
    let mut s = String::from("# 由「星露谷物语模组管理器」生成\n");
    s.push_str(&format!("network_code = {}\n", toml_str(opt.network_code.trim())));
    s.push_str(&format!("server = [{}]\n", toml_str(&server)));
    if !opt.password.trim().is_empty() {
        s.push_str(&format!("password = {}\n", toml_str(opt.password.trim())));
    }
    if !opt.device_name.trim().is_empty() {
        s.push_str(&format!("device_name = {}\n", toml_str(opt.device_name.trim())));
    }
    // 关掉本地控制端口，避免和别的程序抢端口。
    s.push_str("ctrl_port = 0\n");
    s.push_str(&format!("rtx = {}\n", opt.rtx));
    s.push_str(&format!("fec = {}\n", opt.fec));
    if opt.no_broadcast {
        s.push_str("no_broadcast = true\n");
    }
    let path = dir.join("vnt.toml");
    std::fs::write(&path, s)?;
    Ok(path)
}

/// 启动 vnt（需要管理员权限创建虚拟网卡，会弹一次 UAC）。
pub fn vnt_start(opt: &VntOptions) -> Result<()> {
    if opt.network_code.trim().is_empty() {
        return Err(anyhow!("请先填写组网编号（房主和好友填同一个）"));
    }
    let exe = vnt_exe().ok_or_else(|| anyhow!("尚未安装 vnt，请先点「下载 vnt」"))?;
    let conf = write_vnt_conf(opt)?;
    // 用批处理包一层：既能把 vnt 的输出落到日志文件，也方便排查。
    let launcher = vnt_dir().join("start-vnt.cmd");
    let script = format!(
        "@echo off\r\ncd /d \"%~dp0\"\r\n\"{}\" --conf \"{}\" > vnt.log 2>&1\r\n",
        exe.file_name().unwrap_or_default().to_string_lossy(),
        conf.file_name().unwrap_or_default().to_string_lossy()
    );
    std::fs::write(&launcher, script)?;
    run_elevated(&launcher.to_string_lossy(), &[])
}

/// 停止 vnt（进程是管理员权限启动的，停止同样需要授权）。
pub fn vnt_stop() -> Result<()> {
    run_elevated(
        "cmd.exe",
        &["/c".to_string(), "taskkill /IM vnt2_cli.exe /F /T".to_string()],
    )
}

/// vnt 日志尾部（启动失败时给用户看原因）。
pub fn vnt_log_tail(lines: usize) -> String {
    let Ok(text) = std::fs::read_to_string(vnt_dir().join("vnt.log")) else {
        return String::new();
    };
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// 探测 vnt 运行状态与虚拟 IP（会起一个 PowerShell，需在后台线程调用）。
pub fn vnt_probe() -> VntRuntime {
    let script = "\
$r = 0; \
if (Get-Process -Name vnt2_cli -ErrorAction SilentlyContinue) { $r = 1 }; \
$ip = ''; $ifn = ''; \
$n = Get-NetIPAddress -AddressFamily IPv4 -ErrorAction SilentlyContinue | \
     Where-Object { $_.InterfaceAlias -like '*vnt*' } | Select-Object -First 1; \
if ($n) { $ip = $n.IPAddress; $ifn = $n.InterfaceAlias }; \
Write-Output ('RUN=' + $r); \
Write-Output ('IP=' + $ip); \
Write-Output ('IF=' + $ifn)";
    let mut rt = VntRuntime::default();
    let Ok(out) = powershell(script) else {
        return rt;
    };
    for line in out.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("RUN=") {
            rt.running = v.trim() == "1";
        } else if let Some(v) = line.strip_prefix("IP=") {
            let v = v.trim();
            if !v.is_empty() {
                rt.ip = Some(v.to_string());
            }
        } else if let Some(v) = line.strip_prefix("IF=") {
            let v = v.trim();
            if !v.is_empty() {
                rt.iface = Some(v.to_string());
            }
        }
    }
    rt
}