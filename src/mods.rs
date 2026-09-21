//! 模组扫描、启用/禁用。

use crate::model::ModEntry;
use std::path::Path;
use walkdir::WalkDir;

/// 扫描 Mods 目录，返回所有模组。
pub fn scan_mods(mods_path: &Path) -> Vec<ModEntry> {
    let mut entries = Vec::new();
    if !mods_path.is_dir() {
        return entries;
    }

    for entry in WalkDir::new(mods_path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() && entry.file_name() == "manifest.json" {
            let mod_dir = match entry.path().parent() {
                Some(d) => d.to_path_buf(),
                None => continue,
            };
            let folder_name = mod_dir
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();

            let manifest = read_manifest(&entry.path());
            let enabled = !folder_name.starts_with('.');
            let is_content_pack = manifest
                .as_ref()
                .and_then(|m| m.content_pack_for.as_ref())
                .is_some();

            entries.push(ModEntry {
                folder_name,
                path: mod_dir,
                manifest,
                enabled,
                is_content_pack,
            });
        }
    }

    entries.sort_by(|a, b| a.name().to_lowercase().cmp(&b.name().to_lowercase()));
    entries
}

/// 读取并解析 manifest.json（容忍 BOM / JSONC 注释 / 尾逗号）。
fn read_manifest(path: &Path) -> Option<crate::model::Manifest> {
    let bytes = std::fs::read(path).ok()?;
    let text = std::str::from_utf8(&bytes).ok()?;
    crate::model::Manifest::parse(text)
}

/// 切换模组启用状态：通过给文件夹名添加/移除前导 "."。
/// 返回新状态。
pub fn toggle_mod(mod_entry: &ModEntry, enable: bool) -> anyhow::Result<bool> {
    let parent = mod_entry
        .path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("无法获取父目录"))?;
    let name = mod_entry.folder_name.trim_start_matches('.');
    let target = if enable {
        name.to_string()
    } else {
        format!(".{name}")
    };
    let src = parent.join(&mod_entry.folder_name);
    let dst = parent.join(&target);
    if src != dst {
        if dst.exists() {
            anyhow::bail!("目标目录已存在：{}", dst.display());
        }
        std::fs::rename(&src, &dst)?;
    }
    Ok(enable)
}
