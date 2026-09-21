//! 路径检测：Steam 库、星露谷安装目录、SMAPI。

use std::path::{Path, PathBuf};
use std::process::Command;

/// 检测到的游戏环境。
#[derive(Debug, Clone, Default)]
pub struct GameEnv {
    pub game_path: Option<PathBuf>,
    pub smapi_path: Option<PathBuf>,
    pub smapi_version: Option<String>,
    pub mods_path: Option<PathBuf>,
}

/// 从注册表读取 Steam 安装路径（HKCU\Software\Valve\Steam 的 SteamPath）。
pub fn steam_install_path() -> Option<PathBuf> {
    let out = Command::new("reg")
        .args(["query", "HKCU\\Software\\Valve\\Steam", "/v", "SteamPath"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    // 形如 "    SteamPath    REG_SZ    C:\Program Files (x86)\Steam"
    for line in text.lines() {
        if line.contains("SteamPath") {
            let val = line.split("REG_SZ").nth(1).map(|s| s.trim()).unwrap_or("");
            if !val.is_empty() {
                return Some(PathBuf::from(val));
            }
        }
    }
    None
}

/// 读取 libraryfolders.vdf 并解析出所有库目录。
fn parse_libraryfolders(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    // 逐行扫描 "path" 键。
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let t = line.trim();
        if t.starts_with("\"path\"") {
            if let Some(rest) = t.splitn(2, '\t').nth(1).or_else(|| t.splitn(2, "    ").nth(1)) {
                let clean = rest.trim().trim_matches('"');
                if !clean.is_empty() {
                    paths.push(clean.to_string());
                }
            }
        }
    }
    paths
}

/// 所有可能的 Steam 库目录。
pub fn steam_library_paths() -> Vec<PathBuf> {
    let mut result = Vec::new();

    // 1) 注册表 SteamPath。
    if let Some(sp) = steam_install_path() {
        result.push(sp.clone());
        // 新版 libraryfolders.vdf 位置。
        for rel in ["steamapps/libraryfolders.vdf", "config/libraryfolders.vdf"] {
            let vdf = sp.join(rel);
            if let Ok(text) = std::fs::read_to_string(&vdf) {
                for p in parse_libraryfolders(&text) {
                    result.push(PathBuf::from(p));
                }
                break;
            }
        }
    }

    // 2) 常见默认位置。
    for candidate in [
        r"C:\Program Files (x86)\Steam",
        r"C:\Program Files\Steam",
        r"D:\Steam",
        r"D:\SteamLibrary",
        r"E:\SteamLibrary",
        r"D:\Program Files (x86)\Steam",
    ] {
        let p = PathBuf::from(candidate);
        if p.is_dir() && !result.contains(&p) {
            result.push(p);
        }
    }

    result.dedup();
    result
}

/// 在 Steam 库中查找星露谷安装目录。
pub fn find_game_in_steam() -> Option<PathBuf> {
    for lib in steam_library_paths() {
        let common = lib.join("steamapps").join("common");
        for name in ["Stardew Valley", "StardewValley"] {
            let g = common.join(name);
            if g.is_dir() && (g.join("Stardew Valley.exe").is_file() || g.join("StardewValley.exe").is_file()) {
                return Some(g);
            }
        }
    }
    None
}

/// 检查某目录是否包含游戏主程序。
pub fn is_game_dir(p: &Path) -> bool {
    p.join("Stardew Valley.exe").is_file() || p.join("StardewValley.exe").is_file()
}

/// 检测星露谷路径（优先使用用户设置，其次 Steam，再次常见路径）。
pub fn detect_game_path(manual: Option<&str>) -> Option<PathBuf> {
    if let Some(m) = manual {
        let p = PathBuf::from(m);
        if p.is_dir() {
            return Some(p);
        }
    }
    if let Some(g) = find_game_in_steam() {
        return Some(g);
    }
    for candidate in [
        r"C:\Program Files (x86)\Steam\steamapps\common\Stardew Valley",
        r"C:\Program Files (x86)\GOG Galaxy\Games\Stardew Valley",
        r"C:\GOG Games\Stardew Valley",
    ] {
        let p = PathBuf::from(candidate);
        if is_game_dir(&p) {
            return Some(p);
        }
    }
    None
}

/// 检测 SMAPI（StardewModdingAPI.exe）及其版本。
pub fn detect_smapi(game_path: &Path) -> (Option<PathBuf>, Option<String>) {
    let exe = game_path.join("StardewModdingAPI.exe");
    if !exe.is_file() {
        return (None, None);
    }
    let version = smapi_version_from_log().or_else(|| smapi_version_from_folder(game_path));
    (Some(exe), version)
}

/// 从 SMAPI 日志头读取版本。
fn smapi_version_from_log() -> Option<String> {
    let log = smapi_log_path()?;
    let text = std::fs::read_to_string(log).ok()?;
    for line in text.lines().take(40) {
        // 形如 "SMAPI 4.1.10 with Stardew Valley 1.6.14 ..."
        if line.contains("SMAPI") && line.contains("with Stardew Valley") {
            let idx = line.find("SMAPI ")?;
            let rest = &line[idx + 6..];
            let ver: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
            if !ver.is_empty() {
                return Some(ver);
            }
        }
    }
    None
}

/// 从 smapi-internal 目录粗略读取版本。
fn smapi_version_from_folder(game_path: &Path) -> Option<String> {
    let internal = game_path.join("smapi-internal");
    if !internal.is_dir() {
        return None;
    }
    // SMAPI 内部通常有 StardewModdingAPI.dll，版本号难以直接读取，
    // 这里退化为“已安装”。
    Some("已安装".to_string())
}

/// SMAPI 日志路径：%APPDATA%\StardewValley\ErrorLogs\SMAPI-latest.txt。
pub fn smapi_log_path() -> Option<PathBuf> {
    let appdata = PathBuf::from(std::env::var("APPDATA").ok()?);
    let p = appdata.join("StardewValley").join("ErrorLogs").join("SMAPI-latest.txt");
    if p.is_file() {
        return Some(p);
    }
    None
}

/// 汇总检测结果。
pub fn detect(manual_game_path: Option<&str>) -> GameEnv {
    let mut env = GameEnv::default();
    let game = detect_game_path(manual_game_path);
    if let Some(g) = game {
        let mods = g.join("Mods");
        let (smapi, ver) = detect_smapi(&g);
        env.game_path = Some(g);
        env.mods_path = Some(mods);
        env.smapi_path = smapi;
        env.smapi_version = ver;
    }
    env
}
