//! SMAPI 下载与 Nexus Mods API 封装。

use serde_json::Value;

// ---------- SMAPI ----------

/// SMAPI 最新 release 下载地址（GitHub）。
pub fn smapi_latest_release_url() -> Option<String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("StardewModManager")
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .ok()?;
    let resp = client
        .get("https://api.github.com/repos/Pathoschild/SMAPI/releases/latest")
        .send()
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: Value = resp.json().ok()?;
    // 取跨平台安装包（SMAPI-x.y.z-installer.zip），排除 double-zipped 版本。
    for asset in json["assets"].as_array()? {
        let name = asset["name"].as_str()?;
        if name.ends_with("-installer.zip") && !name.contains("double-zipped") {
            return asset["browser_download_url"].as_str().map(|s| s.to_string());
        }
    }
    None
}

/// 下载 SMAPI zip 到本地临时路径，返回路径。
pub fn download_smapi(url: &str) -> Result<std::path::PathBuf, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("StardewModManager")
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(url)
        .send()
        .map_err(|e| format!("下载失败：{e}"))?;
    let bytes = resp.bytes().map_err(|e| e.to_string())?;
    let out = std::env::temp_dir().join("SMAPI-latest.zip");
    std::fs::write(&out, bytes).map_err(|e| e.to_string())?;
    Ok(out)
}

// ---------- Nexus Mods API ----------

/// N 网模组信息。
#[derive(Debug, Clone)]
pub struct NexusMod {
    pub mod_id: u32,
    pub name: String,
    pub summary: String,
    pub author: String,
    pub latest_version: String,
    pub page_url: String,
}

/// 查询 N 网模组详情与最新主文件版本。
pub fn nexus_mod_info(api_key: &str, mod_id: u32) -> Result<NexusMod, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("StardewModManager")
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;

    let detail_url = format!(
        "https://api.nexusmods.com/v1/games/stardewvalley/mods/{}.json",
        mod_id
    );
    let resp = client
        .get(&detail_url)
        .header("apikey", api_key)
        .send()
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("接口返回 {}", resp.status()));
    }
    let json: Value = resp.json().map_err(|e| e.to_string())?;

    let name = json["name"].as_str().unwrap_or("(未知)").to_string();
    let summary = json["summary"].as_str().unwrap_or("").to_string();
    let author = json["user"]["name"].as_str().unwrap_or("").to_string();
    let page_url = format!("https://www.nexusmods.com/stardewvalley/mods/{}", mod_id);

    // 版本号从 files.json 的主文件取。
    let latest_version = nexus_mod_files(api_key, mod_id)
        .ok()
        .and_then(|files| pick_primary_file(&files).map(|f| f.version.clone()))
        .unwrap_or_default();

    Ok(NexusMod {
        mod_id,
        name,
        summary,
        author,
        latest_version,
        page_url,
    })
}

/// N 网模组列表项。
#[derive(Debug, Clone)]
pub struct NexusModSummary {
    pub mod_id: u32,
    pub name: String,
    pub summary: String,
    pub author: String,
    pub version: String,
    pub downloads: u64,
    pub page_url: String,
}

/// 拉取 N 网星露谷模组列表。
/// `list_type`: "trending"（热门）| "latest_added"（最新发布）| 其它（最新更新）。
pub fn nexus_mod_list(api_key: &str, list_type: &str) -> Result<Vec<NexusModSummary>, String> {
    let ep = match list_type {
        "trending" => "trending.json",
        "latest_added" => "latest_added.json",
        _ => "latest_updated.json",
    };
    let url = format!("https://api.nexusmods.com/v1/games/stardewvalley/mods/{}", ep);
    let client = reqwest::blocking::Client::builder()
        .user_agent("StardewModManager")
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;

    let resp = client
        .get(&url)
        .header("apikey", api_key)
        .send()
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("接口返回 {}", resp.status()));
    }
    let arr: Vec<Value> = resp.json().map_err(|e| e.to_string())?;

    let mut list = Vec::new();
    for m in arr {
        let Some(mod_id) = m["mod_id"].as_u64() else {
            continue;
        };
        let mod_id = mod_id as u32;
        let available = m["available"].as_bool().unwrap_or(true);
        let status = m["status"].as_str().unwrap_or("");
        if !available || status == "not_published" {
            continue;
        }
        list.push(NexusModSummary {
            mod_id,
            name: m["name"].as_str().unwrap_or("(未知)").to_string(),
            summary: m["summary"].as_str().unwrap_or("").to_string(),
            author: m["username"].as_str().unwrap_or("").to_string(),
            version: m["version"].as_str().unwrap_or("").to_string(),
            downloads: m["mod_downloads"].as_u64().unwrap_or(0),
            page_url: format!("https://www.nexusmods.com/stardewvalley/mods/{}", mod_id),
        });
    }
    Ok(list)
}

// ---------- N 网文件列表 / nxm 下载 ----------

/// N 网模组文件信息。
#[derive(Debug, Clone)]
pub struct NexusFile {
    pub file_id: u32,
    pub name: String,
    pub version: String,
    pub category_name: String, // MAIN / UPDATE / OPTIONAL 等
    pub size_kb: u64,
    pub is_primary: bool,
}

/// 获取模组的所有文件列表。
pub fn nexus_mod_files(api_key: &str, mod_id: u32) -> Result<Vec<NexusFile>, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("StardewModManager")
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;

    let url = format!(
        "https://api.nexusmods.com/v1/games/stardewvalley/mods/{}/files.json",
        mod_id
    );
    let resp = client
        .get(&url)
        .header("apikey", api_key)
        .send()
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("接口返回 {}", resp.status()));
    }
    let text = resp.text().map_err(|e| e.to_string())?;
    // files.json 是单个 JSON 对象（含 files / file_updates 等字段）。
    let obj: Value = serde_json::from_str(&text).map_err(|e| format!("解析文件列表失败：{e}"))?;

    let mut files = Vec::new();
    if let Some(arr) = obj["files"].as_array() {
        for f in arr {
            let Some(file_id) = f["file_id"].as_u64() else {
                continue;
            };
            files.push(NexusFile {
                file_id: file_id as u32,
                name: f["name"].as_str().unwrap_or("(未知)").to_string(),
                version: f["version"].as_str().unwrap_or("").to_string(),
                category_name: f["category_name"].as_str().unwrap_or("").to_string(),
                size_kb: f["size_kb"].as_u64().unwrap_or(0),
                is_primary: f["is_primary"].as_bool().unwrap_or(false),
            });
        }
    }
    Ok(files)
}

/// 模组的「文件」标签页地址（浏览器下载入口：Manual Download → Slow Download）。
pub fn mod_files_tab_url(mod_id: u32) -> String {
    format!("https://www.nexusmods.com/stardewvalley/mods/{mod_id}?tab=files")
}

/// 从文件列表里挑出「主文件」：优先 is_primary，其次 MAIN 分类中最新的一个，
/// 最后回退第一个文件。
pub fn pick_primary_file(files: &[NexusFile]) -> Option<&NexusFile> {
    files
        .iter()
        .find(|f| f.is_primary)
        .or_else(|| files.iter().find(|f| f.category_name == "MAIN"))
        .or_else(|| files.first())
}

/// 从用户输入解析 mod_id：支持纯数字或 N 网模组链接。
pub fn parse_mod_id(input: &str) -> Option<u32> {
    let s = input.trim();
    if let Ok(n) = s.parse::<u32>() {
        return Some(n);
    }
    // https://www.nexusmods.com/stardewvalley/mods/2400?...
    const MARK: &str = "/mods/";
    let pos = s.find(MARK)?;
    let rest = &s[pos + MARK.len()..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse::<u32>().ok()
}
