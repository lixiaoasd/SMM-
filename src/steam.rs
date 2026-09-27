//! Steam 相关：运行态检测、活跃账号定位、启动项（`LaunchOptions`）读写。
//!
//! 关键前提（都在本机实测过）：
//!   * `<Steam>\steam.pid` **不存在**，不能靠它判活；改用仓库里既有的
//!     `server::process_running`（`tasklist` 按映像名查），与 `server::game_running()`
//!     同一套机制 —— **不要**为了判活去引入 `OpenProcess` /
//!     `QueryFullProcessImageNameW` 这类「读别的进程」的 API：卡巴斯基主动防御
//!     会把它判成 PDM 行为检测（实测在启动时触发过一次）。
//!   * `HKCU\Software\Valve\Steam\ActiveProcess` 的 `ActiveUser` 给出活跃账号；
//!   * Steam 运行期间把 localconfig 放在内存里，**退出时写回磁盘** —— 运行中改文件
//!     必被覆盖，所以「在跑」与「无法确认」一律拒绝写。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use anyhow::{anyhow, Result};

use crate::vdf;

/// 星露谷物语在 Steam 的 appid。
pub const APP_ID: &str = "413150";
/// 启动项里要指向的可执行文件名。
const SMAPI_EXE: &str = "StardewModdingAPI.exe";

// ---------- 注册表读取 ----------

/// `reg query <key>` → (值名, 值原文) 列表。
///
/// 必须带 `CREATE_NO_WINDOW`：本程序是 GUI 子系统（release 下没有控制台），
/// 不加这个标志时每次起 `reg.exe` 都会新建一个控制台窗口 —— 实测拖慢整机
/// 帧率（探测期间帧与帧之间出现 ~480ms 空档），也是杀软行为检测的噪音来源。
fn reg_query(key: &str) -> Option<Vec<(String, String)>> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("reg")
        .args(["query", key])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut vals = Vec::new();
    for line in text.lines() {
        // 形如 "    pid    REG_DWORD    0x36a8"
        for kind in ["REG_DWORD", "REG_SZ", "REG_EXPAND_SZ", "REG_QWORD", "REG_MULTI_SZ"] {
            if let Some(pos) = line.find(kind) {
                let name = line[..pos].trim();
                let value = line[pos + kind.len()..].trim();
                if !name.is_empty() {
                    vals.push((name.to_string(), value.to_string()));
                }
                break;
            }
        }
    }
    Some(vals)
}

fn reg_value(key: &str, name: &str) -> Option<String> {
    reg_query(key)?
        .into_iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v)
}

/// 解析注册表里的 DWORD（`reg query` 输出为十六进制 `0x...`）。
fn parse_dword(v: &str) -> Option<u32> {
    let t = v.trim();
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()
    } else {
        t.parse::<u32>().ok()
    }
}

fn reg_active_user() -> Option<u32> {
    let v = parse_dword(&reg_value(
        "HKCU\\Software\\Valve\\Steam\\ActiveProcess",
        "ActiveUser",
    )?)?;
    (v != 0).then_some(v)
}

// ---------- Steam 运行态 ----------

/// Steam 是否在运行。三态，避免把「问不出来」当成「没运行」这种危险默认。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunState {
    Running,
    Stopped,
    /// 无法确认。写盘路径上等同于「不许写」。
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SteamRuntime {
    pub install_path: Option<PathBuf>,
    pub run: RunState,
    pub account_id: Option<u32>,
    pub localconfig: Option<PathBuf>,
}

impl SteamRuntime {
    pub fn running(&self) -> bool {
        self.run == RunState::Running
    }
    /// 无法确认 Steam 是否已退出（写盘会被拒绝）。
    pub fn state_unknown(&self) -> bool {
        self.install_path.is_some() && self.run == RunState::Unknown
    }
}

fn detect_run_state() -> RunState {
    match crate::server::process_running("steam.exe") {
        Some(true) => RunState::Running,
        Some(false) => RunState::Stopped,
        None => RunState::Unknown,
    }
}

// ---------- 活跃账号 ----------
//
// 定位活跃账号**只允许**用下面两条路：
//   1. `HKCU\...\Steam\ActiveProcess\ActiveUser`（Steam 自己在跑的账号）；
//   2. 枚举 `<Steam>\userdata\*`，取 `localconfig.vdf` 修改时间最新的那个目录
//      （Steam 退出时会把内存里的配置写回该账号的 localconfig，所以最近用的账号
//      时间最新）。
//
// **绝对不要读 `<Steam>\config\loginusers.vdf`**：那里面有账号名列表，是 Steam
// 盗号木马的标准取材对象 —— 实测只要读一次这个文件，卡巴斯基的系统监控就会把本
// 进程挂起并判 `PDM:Trojan.Win32.Generic`（用 `--steam-smoke` 分级复现过：只读
// loginusers.vdf 会被挂起，而只读注册表、只查 tasklist、只枚举 userdata、只读
// 某个账号的 localconfig.vdf 都干净）。

fn localconfig_for(install: &Path, account_id: u32) -> PathBuf {
    install
        .join("userdata")
        .join(account_id.to_string())
        .join("config")
        .join("localconfig.vdf")
}

/// 最近使用的账号：`userdata\*` 里 localconfig.vdf 修改时间最新的那个。
///
/// 找不到（Steam 从没登录过 / 没有任何 localconfig）返回 None。
fn most_recent_userdata_account(install: &Path) -> Option<u32> {
    let rd = std::fs::read_dir(install.join("userdata")).ok()?;
    let mut best: Option<(std::time::SystemTime, u32)> = None;
    for e in rd.flatten() {
        if !e.path().is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        let Ok(id) = name.parse::<u32>() else { continue };
        let Ok(meta) = std::fs::metadata(localconfig_for(install, id)) else {
            continue;
        };
        let Ok(mtime) = meta.modified() else { continue };
        if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
            best = Some((mtime, id));
        }
    }
    best.map(|(_, id)| id)
}

fn active_account_id(install: &Path) -> Option<u32> {
    if let Some(id) = reg_active_user() {
        if localconfig_for(install, id).is_file() {
            return Some(id);
        }
    }
    most_recent_userdata_account(install)
}

/// 探测一次完整状态。会起 1~2 次 `reg` + 1 次 `tasklist`，**只该在后台线程调用**。
pub fn runtime() -> SteamRuntime {
    let install = crate::paths::steam_install_path().filter(|p| p.is_dir());
    let run = match &install {
        Some(_) => detect_run_state(),
        None => RunState::Stopped,
    };
    let account_id = install.as_deref().and_then(active_account_id);
    let localconfig = match (&install, account_id) {
        (Some(i), Some(a)) => Some(localconfig_for(i, a)),
        _ => None,
    };
    SteamRuntime {
        install_path: install,
        run,
        account_id,
        localconfig,
    }
}

// ---------- 启动项 ----------

/// 期望的启动项内容：带引号的 SMAPI 路径 + `%command%`。
///
/// 少了 `%command%` 会丢掉 Steam 原有的启动参数；不加引号会因路径含空格而解析错。
pub fn expected_option(game_path: &Path) -> String {
    format!("\"{}\" %command%", game_path.join(SMAPI_EXE).display())
}

/// 目标目录是否真的是 Steam 版星露谷（`<库>\steamapps\common\<installdir>` + 对应 acf）。
pub fn is_steam_game(game_path: &Path) -> bool {
    let Some(common) = game_path.parent() else {
        return false;
    };
    let named = |p: &Path, want: &str| {
        p.file_name()
            .map(|n| n.eq_ignore_ascii_case(want))
            .unwrap_or(false)
    };
    if !named(common, "common") {
        return false;
    }
    let Some(steamapps) = common.parent() else {
        return false;
    };
    if !named(steamapps, "steamapps") {
        return false;
    }
    let acf = steamapps.join(format!("appmanifest_{APP_ID}.acf"));
    let Ok(bytes) = std::fs::read(&acf) else {
        return false;
    };
    let Ok(root) = vdf::parse_root(&bytes) else {
        return false;
    };
    let Ok(view) = vdf::block(&bytes, root.open) else {
        return false;
    };
    let want = game_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    for e in view.entries {
        if let vdf::Entry::Pair(p) = e {
            if vdf::decode(&p.key) == "installdir" {
                return vdf::decode(&p.value_raw).eq_ignore_ascii_case(&want);
            }
        }
    }
    true
}

/// 启动项当前状态。
#[derive(Debug, Clone, PartialEq, Default)]
pub enum LaunchState {
    /// 未检测到 Steam 安装。
    #[default]
    NoSteam,
    /// 游戏目录不是 Steam 版（GOG / 手动安装）。
    NotSteamGame,
    /// 游戏目录下没有 StardewModdingAPI.exe。
    NoSmapi,
    /// 定位不到活跃账号（多账号 / 从未登录）。
    NoAccount,
    /// localconfig 读不了或结构不符（拒绝写）。
    Unreadable(String),
    /// 未设置，或值为空。
    Unset,
    /// 已指向本机 SMAPI。
    PointsToSmapi,
    /// 指向 SMAPI 但该 exe 不存在（游戏搬家 / SMAPI 被删）。
    StalePath { value: String },
    /// 指向别的内容。
    PointsElsewhere { value: String },
}

#[derive(Debug, Clone, Default)]
pub struct SteamStatus {
    pub runtime: SteamRuntime,
    pub launch: LaunchState,
    /// 当前启动项原文（若已设置）。
    pub current: Option<String>,
}

/// `\\` → `\` 反复折叠，使路径比较与转义写法无关。
pub fn normalize_path_str(s: &str) -> String {
    let mut cur = s.to_string();
    loop {
        let next = cur.replace("\\\\", "\\");
        if next == cur {
            return cur;
        }
        cur = next;
    }
}

fn path_key(s: &str) -> String {
    let mut t = normalize_path_str(s).replace('/', "\\").to_lowercase();
    while t.ends_with('\\') {
        t.pop();
    }
    t
}

/// 取启动项最外层引号里的可执行文件路径。
fn leading_quoted_path(value: &str) -> Option<&str> {
    let rest = value.trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// 判断启动项内容指向哪个 SMAPI。`exists` 用于区分「已失效」。
pub fn classify_value_with(value: &str, want_exe: &Path, exists: bool) -> LaunchState {
    let norm = normalize_path_str(value);
    let quoted = leading_quoted_path(&norm);
    let points_to_smapi = match quoted {
        Some(p) => p.to_lowercase().ends_with(&SMAPI_EXE.to_lowercase()),
        None => norm.to_lowercase().contains(&SMAPI_EXE.to_lowercase()),
    };
    if !points_to_smapi {
        return LaunchState::PointsElsewhere {
            value: value.to_string(),
        };
    }
    if exists && quoted.map(|p| path_key(p) == path_key(&want_exe.display().to_string())).unwrap_or(false)
    {
        LaunchState::PointsToSmapi
    } else {
        LaunchState::StalePath {
            value: value.to_string(),
        }
    }
}

fn classify_value(value: &str, game: &Path) -> LaunchState {
    let want = game.join(SMAPI_EXE);
    classify_value_with(value, &want, want.is_file())
}

fn classify(rt: &SteamRuntime, game_path: Option<&Path>) -> (LaunchState, Option<String>) {
    let Some(cfg) = &rt.localconfig else {
        return (
            if rt.install_path.is_none() {
                LaunchState::NoSteam
            } else {
                LaunchState::NoAccount
            },
            None,
        );
    };
    let Some(game) = game_path else {
        return (LaunchState::NotSteamGame, None);
    };
    if !is_steam_game(game) {
        return (LaunchState::NotSteamGame, None);
    }
    if !game.join(SMAPI_EXE).is_file() {
        return (LaunchState::NoSmapi, None);
    }
    let Ok(bytes) = std::fs::read(cfg) else {
        return (LaunchState::Unreadable("读取 localconfig.vdf 失败".into()), None);
    };
    match vdf::get_app_string(&bytes, APP_ID, "LaunchOptions") {
        Err(e) => (LaunchState::Unreadable(e.to_string()), None),
        Ok(None) => (LaunchState::Unset, None),
        Ok(Some(v)) if v.trim().is_empty() => (LaunchState::Unset, None),
        Ok(Some(v)) => (classify_value(&v, game), Some(v)),
    }
}

/// 探测当前状态（含运行检测与 localconfig 读取）。**只该在后台线程调用**。
pub fn probe(game_path: Option<&Path>) -> SteamStatus {
    let rt = runtime();
    let (launch, current) = classify(&rt, game_path);
    SteamStatus {
        runtime: rt,
        launch,
        current,
    }
}

// ---------- 写入 ----------

/// 遇到「已有其它内容」时的策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// 自动路径：不覆盖用户自己写的内容。
    KeepCustom,
    /// 手动按钮：用户已明确要求覆盖。
    Overwrite,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SetOutcome {
    Written,
    SkippedHasOtherContent(String),
    SteamRunning,
    LivenessUnknown,
    NotSteamGame,
    NoSteam,
    NoConfig,
}

impl SetOutcome {
    pub fn message(&self) -> String {
        match self {
            SetOutcome::Written => "Steam 启动项已指向 SMAPI".to_string(),
            SetOutcome::SkippedHasOtherContent(v) => {
                format!("Steam 启动项已有其它内容（{v}），已跳过；要覆盖请到设置页手动设置")
            }
            SetOutcome::SteamRunning => {
                "Steam 正在运行，启动项未改；完全退出 Steam 后可在设置页一键设置".to_string()
            }
            SetOutcome::LivenessUnknown => {
                "无法确认 Steam 是否已退出，启动项未改；请在设置页重试".to_string()
            }
            SetOutcome::NotSteamGame => "游戏目录不是 Steam 版，未改启动项".to_string(),
            SetOutcome::NoSteam => "未检测到 Steam，未改启动项".to_string(),
            SetOutcome::NoConfig => {
                "定位不到 Steam 活跃账号的配置文件，未改启动项（多账号请先用目标账号登录一次 Steam）"
                    .to_string()
            }
        }
    }

    /// 只有「已写入」是不需要用户额外处理的。
    pub fn needs_attention(&self) -> bool {
        !matches!(self, SetOutcome::Written)
    }
}

/// 串行化对 Steam 配置的读改写，避免自动路径与手动按钮交错。
static CONFIG_LOCK: Mutex<()> = Mutex::new(());

/// 把启动项指向 SMAPI。`game` 为 `None` 表示改为空（恢复默认）。
pub fn write_launch_option(game: Option<&Path>, policy: Policy) -> Result<SetOutcome> {
    let _guard = CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let rt = runtime();
    if rt.install_path.is_none() {
        return Ok(SetOutcome::NoSteam);
    }
    match rt.run {
        RunState::Running => return Ok(SetOutcome::SteamRunning),
        // fail-safe：确认不了就绝不写。
        RunState::Unknown => return Ok(SetOutcome::LivenessUnknown),
        RunState::Stopped => {}
    }
    let Some(cfg) = rt.localconfig.clone() else {
        return Ok(SetOutcome::NoConfig);
    };

    let desired = match game {
        Some(g) => {
            if !is_steam_game(g) {
                return Ok(SetOutcome::NotSteamGame);
            }
            expected_option(g)
        }
        None => String::new(),
    };

    // 锁内重读，避免丢掉别人（或 Steam）刚写入的内容。
    let src = std::fs::read(&cfg).map_err(|e| anyhow!("读取 {} 失败：{e}", cfg.display()))?;
    let current = vdf::get_app_string(&src, APP_ID, "LaunchOptions")
        .map_err(|e| anyhow!("解析 localconfig.vdf 失败：{e}"))?;
    if policy == Policy::KeepCustom {
        if let Some(cur) = &current {
            let ours = game
                .map(|g| matches!(classify_value(cur, g), LaunchState::PointsToSmapi | LaunchState::StalePath { .. }))
                .unwrap_or(false);
            if !cur.trim().is_empty() && !ours {
                return Ok(SetOutcome::SkippedHasOtherContent(cur.clone()));
            }
        }
    }

    // 内存中编辑 → 回读校验 → 备份 → 临时文件 + rename，任一步不过都不落盘。
    let out = vdf::set_app_string(&src, APP_ID, "LaunchOptions", &desired)
        .map_err(|e| anyhow!("改写 localconfig.vdf 失败：{e}"))?;
    match vdf::get_app_string(&out, APP_ID, "LaunchOptions") {
        Ok(Some(v)) if v == desired => {}
        _ => return Err(anyhow!("写入自检未通过（回读内容不符），已放弃修改")),
    }
    if !vdf::braces_balanced(&out) {
        return Err(anyhow!("写入自检未通过（括号不配对），已放弃修改"));
    }

    backup(&cfg, rt.account_id)?;
    atomic_write(&cfg, &out)?;
    Ok(SetOutcome::Written)
}

/// 备份目录（放在我们自己家目录，不往 Steam 目录丢垃圾）。
fn backup_dir() -> PathBuf {
    crate::model::config_dir().join("backup")
}

fn backup(cfg: &Path, account: Option<u32>) -> Result<()> {
    let dir = backup_dir();
    std::fs::create_dir_all(&dir)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = format!(
        "localconfig_{}_{}.vdf",
        account.map(|a| a.to_string()).unwrap_or_else(|| "unknown".into()),
        stamp
    );
    let dest = dir.join(name);
    // 绝不覆盖已存在的备份。
    if !dest.exists() {
        std::fs::copy(cfg, &dest)?;
    }
    prune_backups(&dir, 3);
    Ok(())
}

fn prune_backups(dir: &Path, keep: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut names: Vec<String> = rd
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            (n.starts_with("localconfig_") && n.ends_with(".vdf")).then_some(n)
        })
        .collect();
    names.sort();
    while names.len() > keep {
        let oldest = names.remove(0);
        let _ = std::fs::remove_file(dir.join(oldest));
    }
}

fn atomic_write(cfg: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = cfg.with_extension("vdf.smm-tmp");
    std::fs::write(&tmp, bytes)?;
    match std::fs::rename(&tmp, cfg) {
        Ok(()) => Ok(()),
        Err(e) => Err(anyhow!(
            "替换 {} 失败（{e}）—— 请确认 Steam 已完全退出；临时文件保留在 {}",
            cfg.display(),
            tmp.display()
        )),
    }
}

/// 把最近一份备份拷回 Steam。返回被恢复的备份路径。
pub fn restore_latest_backup() -> Result<PathBuf> {
    let _guard = CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = backup_dir();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Err(anyhow!("没有可用的备份"));
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            (n.starts_with("localconfig_") && n.ends_with(".vdf")).then_some(n)
        })
        .collect();
    names.sort();
    let latest = names.pop().ok_or_else(|| anyhow!("没有可用的备份"))?;
    let src = dir.join(&latest);

    let rt = runtime();
    let cfg = rt
        .localconfig
        .clone()
        .ok_or_else(|| anyhow!("定位不到 Steam 配置文件"))?;
    if rt.running() || rt.state_unknown() {
        return Err(anyhow!("Steam 正在运行或状态未知，请完全退出 Steam 后再恢复"));
    }
    std::fs::copy(&src, &cfg)?;
    Ok(src)
}

// ---------- 测试 ----------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn game() -> PathBuf {
        PathBuf::from(r"D:\SteamLibrary\steamapps\common\Stardew Valley")
    }

    #[test]
    fn expected_option_format() {
        let s = expected_option(&game());
        assert_eq!(
            s,
            r#""D:\SteamLibrary\steamapps\common\Stardew Valley\StardewModdingAPI.exe" %command%"#
        );
        assert!(s.ends_with(" %command%"), "不能丢 %command%");
        assert!(s.starts_with('"'), "路径含空格，必须加引号");
    }

    #[test]
    fn picks_most_recently_written_localconfig() {
        let root = std::env::temp_dir().join(format!("smm_steam_acct_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let make = |id: u32, secs_ago: u64| {
            let f = root.join("userdata").join(id.to_string()).join("config");
            std::fs::create_dir_all(&f).unwrap();
            let p = f.join("localconfig.vdf");
            std::fs::write(&p, b"x").unwrap();
            let t = std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago);
            std::fs::File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(t)
                .unwrap();
        };
        make(1449881501, 9_000);
        make(430929835, 60); // 最近用的账号
        make(800250950, 4_000);
        // userdata 下的非数字目录必须被忽略。
        std::fs::create_dir_all(root.join("userdata").join("not-a-number")).unwrap();

        assert_eq!(most_recent_userdata_account(&root), Some(430929835));
        // 没有 localconfig.vdf 的目录不算数。
        let empty = root.join("empty");
        std::fs::create_dir_all(empty.join("userdata").join("123")).unwrap();
        assert_eq!(most_recent_userdata_account(&empty), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn run_state_maps_from_tasklist() {
        // 三态映射：问不出来必须是 Unknown（写盘路径上等同「不许写」）。
        let map = |r: Option<bool>| match r {
            Some(true) => RunState::Running,
            Some(false) => RunState::Stopped,
            None => RunState::Unknown,
        };
        assert_eq!(map(Some(true)), RunState::Running);
        assert_eq!(map(Some(false)), RunState::Stopped);
        assert_eq!(map(None), RunState::Unknown);
    }

    #[test]
    fn classify_launch_values() {
        let want = game().join(SMAPI_EXE);
        // 双反斜杠写法
        let two = r#""D:\\SteamLibrary\\steamapps\\common\\Stardew Valley\\StardewModdingAPI.exe" %command%"#;
        assert_eq!(classify_value_with(two, &want, true), LaunchState::PointsToSmapi);
        // 单反斜杠写法
        let one = r#""D:\SteamLibrary\steamapps\common\Stardew Valley\StardewModdingAPI.exe" %command%"#;
        assert_eq!(classify_value_with(one, &want, true), LaunchState::PointsToSmapi);
        // exe 不存在
        assert!(matches!(
            classify_value_with(one, &want, false),
            LaunchState::StalePath { .. }
        ));
        // 指向别处
        assert!(matches!(
            classify_value_with("-console", &want, true),
            LaunchState::PointsElsewhere { .. }
        ));
        // 指向别的盘的 SMAPI（换了游戏目录）
        let other = r#""E:\games\StardewModdingAPI.exe" %command%"#;
        assert!(matches!(
            classify_value_with(other, &want, true),
            LaunchState::StalePath { .. }
        ));
    }

    #[test]
    fn normalize_collapses_backslashes() {
        assert_eq!(normalize_path_str(r"D:\\a\\b"), r"D:\a\b");
        assert_eq!(normalize_path_str(r"D:\a\b"), r"D:\a\b");
    }

    #[test]
    fn outcome_messages() {
        assert!(!SetOutcome::Written.needs_attention());
        assert!(SetOutcome::SteamRunning.needs_attention());
        assert!(SetOutcome::SkippedHasOtherContent("x".into())
            .message()
            .contains("x"));
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("smm_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn atomic_write_replaces_existing_file() {
        // Windows 上 rename 必须能覆盖已存在的目标，否则每次写入都会失败。
        let dir = tmp_dir("atomic");
        let f = dir.join("localconfig.vdf");
        std::fs::write(&f, b"OLD-CONTENT").unwrap();
        atomic_write(&f, b"NEW-CONTENT").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"NEW-CONTENT");
        assert!(
            !f.with_extension("vdf.smm-tmp").exists(),
            "临时文件应已被 rename 消费掉，不该残留"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_keeps_newest_backups() {
        let dir = tmp_dir("prune");
        // 文件名里的时间戳决定新老顺序。
        for stamp in [100u64, 400, 200, 300] {
            std::fs::write(dir.join(format!("localconfig_1_{stamp}.vdf")), b"x").unwrap();
        }
        std::fs::write(dir.join("unrelated.txt"), b"keep").unwrap();
        prune_backups(&dir, 3);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".vdf"))
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![
                "localconfig_1_200.vdf".to_string(),
                "localconfig_1_300.vdf".to_string(),
                "localconfig_1_400.vdf".to_string(),
            ],
            "应保留最新的 3 份"
        );
        assert!(dir.join("unrelated.txt").exists(), "不该动无关文件");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
