//! 数据模型与配置持久化。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 默认镜像源：云端模组库（朋友开箱即用，无需自建服务器）。
/// 自建镜像时可在设置页改为 http://localhost:8770 等地址。
pub const DEFAULT_MIRROR_URL: &str = "http://116.62.231.162";

/// SMAPI 模组的 manifest.json 结构（仅取用到的字段）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Manifest {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Author", default)]
    pub author: String,
    #[serde(rename = "Version", default)]
    pub version: String,
    #[serde(rename = "Description", default)]
    pub description: String,
    #[serde(rename = "UniqueID", default)]
    pub unique_id: String,
    #[serde(rename = "EntryDll", default)]
    pub entry_dll: String,
    #[serde(rename = "ContentPackFor", default)]
    pub content_pack_for: Option<ContentPackFor>,
    /// 必需前置模组（Dependencies；IsRequired 缺省视为 true）。
    #[serde(rename = "Dependencies", default)]
    pub dependencies: Vec<ManifestDep>,
    /// 冲突模组（Conflicts：匹配 UniqueID 的正则，SMAPI 大小写不敏感）。
    #[serde(rename = "Conflicts", default)]
    pub conflicts: Vec<String>,
}

/// manifest Dependencies 数组元素。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestDep {
    #[serde(rename = "UniqueID", default)]
    pub unique_id: String,
    /// SMAPI 语义：缺省视为必需。
    #[serde(rename = "IsRequired", default = "default_true")]
    pub is_required: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContentPackFor {
    #[serde(rename = "UniqueID", default)]
    pub unique_id: String,
}

/// 扫描得到的一个模组条目。
#[derive(Debug, Clone)]
pub struct ModEntry {
    /// Mods 目录下（或子目录下）模组文件夹名，例如 "ContentPatcher"。
    pub folder_name: String,
    /// 模组文件夹绝对路径。
    pub path: PathBuf,
    /// 解析出的 manifest（可能缺失）。
    pub manifest: Option<Manifest>,
    /// 是否启用（文件夹名不以 "." 开头）。
    pub enabled: bool,
    /// 是否内容包（Content Patcher 等）。
    pub is_content_pack: bool,
}

impl ModEntry {
    pub fn name(&self) -> String {
        self.manifest
            .as_ref()
            .map(|m| m.name.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.folder_name.clone())
    }

    pub fn unique_id(&self) -> String {
        self.manifest
            .as_ref()
            .map(|m| m.unique_id.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("folder:{}", self.folder_name))
    }

    pub fn version(&self) -> String {
        self.manifest
            .as_ref()
            .map(|m| m.version.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "-".to_string())
    }

    pub fn author(&self) -> String {
        self.manifest
            .as_ref()
            .map(|m| m.author.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "-".to_string())
    }
}

/// 应用设置（持久化到配置文件）。
///
/// struct 级 `#[serde(default)]`：旧版 settings.json 缺少字段或含有已删除字段时
/// 都能正常加载（多余字段忽略，缺失字段取 Default）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// 星露谷游戏根目录（含 Stardew Valley.exe / StardewModdingAPI.exe）。
    pub game_path: Option<String>,
    /// Nexus Mods API key（用于查询 N 网模组信息）。
    pub nexus_api_key: String,
    /// 用户是否已在系统浏览器完成 N 网登录引导（仅控制引导卡显示）。
    pub nexus_login_done: bool,
    /// 镜像源地址（默认云端模组库，可在设置页改为自建地址）。
    pub mirror_url: String,
    /// 一键开服与房主助手配置（与游戏内 HostKit 插件共用一套字段）。
    pub host: crate::server::HostKitConfig,
    /// vnt 内网穿透配置。
    pub vnt: crate::server::VntOptions,
}

impl Manifest {
    /// 宽容解析 manifest.json：SMAPI 作者常用 VS 保存（带 UTF-8 BOM），
    /// 部分作者（如 LeFauxMatt）写入 JSONC 块注释，严格 serde 会整体失败，
    /// 这里剥掉 BOM/注释/尾逗号后再解析。
    pub fn parse(text: &str) -> Option<Manifest> {
        serde_json::from_str(&jsonc_to_json(text)).ok()
    }
}

/// 把「带注释/BOM/尾逗号」的宽松 JSON（JSONC）转成严格 JSON。
/// 正确识别字符串字面量，不会误删字符串里的 `https://` 等内容。
fn jsonc_to_json(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_string = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            '\u{feff}' => {
                i += 1; // 跳过 BOM
            }
            '"' => {
                in_string = true;
                out.push(c);
                i += 1;
            }
            '/' if i + 1 < chars.len() && chars[i + 1] == '/' => {
                i += 2;
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if i + 1 < chars.len() && chars[i + 1] == '*' => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    // 去掉对象/数组结束前的尾逗号：",\s*}" -> "}"、",\s*]" -> "]"。
    let s: Vec<char> = out.chars().collect();
    let mut res = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == ',' {
            let mut j = i + 1;
            while j < s.len() && s[j].is_whitespace() {
                j += 1;
            }
            if j < s.len() && (s[j] == '}' || s[j] == ']') {
                i += 1; // 丢弃逗号，空白保留
                continue;
            }
        }
        res.push(s[i]);
        i += 1;
    }
    res
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            game_path: None,
            nexus_api_key: String::new(),
            nexus_login_done: false,
            mirror_url: DEFAULT_MIRROR_URL.to_string(),
            host: crate::server::HostKitConfig::default(),
            vnt: crate::server::VntOptions::default(),
        }
    }
}

/// Windows 上的 %APPDATA% 目录（Roaming）。
pub fn appdata() -> PathBuf {
    std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// 配置根目录：`%APPDATA%\StardewModManager`。
pub fn config_dir() -> PathBuf {
    appdata().join("StardewModManager")
}

impl Settings {
    /// 配置文件路径：`%APPDATA%\StardewModManager\settings.json`。
    pub fn path() -> PathBuf {
        appdata().join("StardewModManager").join("settings.json")
    }

    pub fn load() -> Self {
        let p = Self::path();
        if let Ok(text) = std::fs::read_to_string(&p) {
            if let Ok(s) = serde_json::from_str::<Settings>(&text) {
                return s;
            }
        }
        Settings::default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let p = Self::path();
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&p, text)?;
        Ok(())
    }
}
