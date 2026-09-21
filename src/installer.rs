//! 模组安装（解压 zip）、更新，以及 SMAPI 下载。

use anyhow::Result;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// 全局安装锁：镜像直装 / 浏览器监控 / 本地 zip 可能同时触发安装，
/// 串行化对同一 Mods 目录的写入，避免并发解压互相覆盖。
static INSTALL_LOCK: Mutex<()> = Mutex::new(());
static EXTRACT_SEQ: AtomicU64 = AtomicU64::new(0);

/// 解压一个压缩包（zip）到 Mods 目录，自动处理“外层套版本号文件夹”的情况。
/// 返回安装的模组名列表。
pub fn install_archive(archive: &Path, mods_dir: &Path) -> Result<Vec<String>> {
    let _guard = INSTALL_LOCK.lock().unwrap();
    let file = std::fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)?;

    // 先解压到本次安装独占的临时目录（共享目录会在并发安装时互相删除）。
    let seq = EXTRACT_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!(
        "stardew_extract_{}_{}_{}",
        std::process::id(),
        seq,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&tmp)?;

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry.name().to_string();
        // 防止 zip slip 路径穿越。
        let out_path = sanitize_join(&tmp, &name)?;
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            std::fs::write(&out_path, &buf)?;
        }
    }

    // 找出包含 manifest.json 的目录作为“模组根”。
    let roots = find_mod_roots(&tmp);
    if roots.is_empty() {
        // 无 manifest：可能是“汉化覆盖包”（zip 内只有 <模组名>/i18n/zh.json
        // 之类的语言文件，需要合并进已安装的同名模组目录）。
        match try_install_i18n_overlay(&tmp, mods_dir)? {
            Some(names) => {
                let _ = std::fs::remove_dir_all(&tmp);
                return Ok(names);
            }
            None => {
                let _ = std::fs::remove_dir_all(&tmp);
                anyhow::bail!("压缩包内未找到任何含 manifest.json 的模组目录");
            }
        }
    }

    std::fs::create_dir_all(mods_dir)?;
    let mut installed = Vec::new();
    for root in roots {
        let name = root
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let dest = mods_dir.join(&name);
        // 已存在则覆盖式更新（保留 config.json）。
        if dest.exists() {
            copy_dir_merge(&root, &dest)?;
        } else {
            copy_dir(&root, &dest)?;
        }
        installed.push(name);
    }

    let _ = std::fs::remove_dir_all(&tmp);
    Ok(installed)
}

/// 安全拼接，避免路径穿越。
fn sanitize_join(base: &Path, rel: &str) -> Result<PathBuf> {
    let clean = rel.replace('\\', "/");
    let p = PathBuf::from(&clean);
    let mut out = base.to_path_buf();
    for comp in p.components() {
        match comp {
            std::path::Component::Normal(c) => out.push(c),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => anyhow::bail!("非法路径：{}", rel),
            _ => {}
        }
    }
    Ok(out)
}

/// 递归查找含 manifest.json 的目录。
fn find_mod_roots(base: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if !base.is_dir() {
        return roots;
    }
    // 若当前目录直接含 manifest，它就是根。
    if base.join("manifest.json").is_file() {
        roots.push(base.to_path_buf());
        return roots;
    }
    // 否则深入子目录（限制两层，避免把 i18n 子目录误判）。
    for e1 in std::fs::read_dir(base).into_iter().flatten().flatten() {
        let p1 = e1.path();
        if p1.is_dir() && p1.join("manifest.json").is_file() {
            roots.push(p1);
        } else if p1.is_dir() {
            for e2 in std::fs::read_dir(&p1).into_iter().flatten().flatten() {
                let p2 = e2.path();
                if p2.is_dir() && p2.join("manifest.json").is_file() {
                    roots.push(p2);
                }
            }
        }
    }
    roots
}

/// 识别并安装“汉化/语言覆盖包”：整个 zip 不含 manifest，只有形如
/// `<任意包装层>/<模组目录名>/i18n/<lang>.json` 的文件（N 网汉化包的典型结构）。
/// 把语言文件合并进 Mods 下已存在的同名模组目录。
/// 返回 None 表示不是覆盖包；目标主模组未安装时报错（压缩包由调用方保留）。
fn try_install_i18n_overlay(base: &Path, mods_dir: &Path) -> Result<Option<Vec<String>>> {
    /// (源文件, Mods 下模组目录名, 模组目录内的相对路径)。
    struct OverlayFile {
        src: PathBuf,
        mod_folder: String,
        rel_in_mod: PathBuf,
    }

    let mut files: Vec<PathBuf> = Vec::new();
    collect_files(base, &mut files)?;
    if files.is_empty() {
        return Ok(None);
    }

    let mut overlays: Vec<OverlayFile> = Vec::new();
    for f in &files {
        let rel = match f.strip_prefix(base) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };
        let comps: Vec<String> = rel
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        // 找到 i18n 目录段，其前一段就是 Mods 下的模组目录名。
        let pos = comps.iter().position(|c| c.eq_ignore_ascii_case("i18n"));
        let valid = pos
            .map(|p| {
                p >= 1
                    && p + 1 < comps.len()
                    && f.extension()
                        .map(|e| e.eq_ignore_ascii_case("json"))
                        .unwrap_or(false)
            })
            .unwrap_or(false);
        if !valid {
            // 含有任何不属于 i18n 语言文件的内容，则不是覆盖包。
            return Ok(None);
        }
        let p = pos.unwrap();
        let mod_folder = comps[p - 1].clone();
        let rel_in_mod: PathBuf = comps[p..].iter().collect();
        overlays.push(OverlayFile {
            src: f.clone(),
            mod_folder,
            rel_in_mod,
        });
    }

    // 所有目标模组目录必须已安装，否则给出明确的主模组提示。
    let mut folders: Vec<&str> = overlays.iter().map(|o| o.mod_folder.as_str()).collect();
    folders.sort_unstable();
    folders.dedup();
    let missing: Vec<&str> = folders
        .iter()
        .filter(|name| !mods_dir.join(*name).is_dir())
        .copied()
        .collect();
    if !missing.is_empty() {
        anyhow::bail!(
            "汉化包需要先安装主模组：{}（压缩包已保留）",
            missing.join("、")
        );
    }

    for o in &overlays {
        let dest = mods_dir.join(&o.mod_folder).join(&o.rel_in_mod);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&o.src, &dest)?;
    }
    let names = folders
        .into_iter()
        .map(|n| format!("{n} 汉化"))
        .collect();
    Ok(Some(names))
}

/// 递归收集目录下全部文件。
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let p = e.path();
        if p.is_dir() {
            collect_files(&p, out)?;
        } else {
            out.push(p);
        }
    }
    Ok(())
}

/// 递归拷贝目录。
fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let from = e.path();
        let to = dst.join(e.file_name());
        if from.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// 合并拷贝：目标已存在时，仅覆盖同名文件，保留目标中多出的 config.json 等。
fn copy_dir_merge(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let from = e.path();
        let to = dst.join(e.file_name());
        if from.is_dir() {
            copy_dir_merge(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// 一键安装 SMAPI：下载最新版安装包，解压，调用官方安装器。
pub fn install_smapi(game_path: &Path) -> Result<String> {
    // 1) 从 GitHub 获取最新版下载地址。
    let url = crate::web::smapi_latest_release_url()
        .ok_or_else(|| anyhow::anyhow!("无法获取 SMAPI 下载地址（检查网络后重试）"))?;

    // 2) 下载安装包。
    let zip_path = crate::web::download_smapi(&url)
        .map_err(|e| anyhow::anyhow!("下载失败：{}", e))?;

    // 3) 解压到临时目录。
    let extract = std::env::temp_dir().join(format!("stardew_smapi_{}", std::process::id()));
    extract_zip(&zip_path, &extract)?;

    // 4) 定位官方安装器（internal/windows/SMAPI.Installer.exe）。
    let installer_exe = find_smapi_installer(&extract)
        .ok_or_else(|| anyhow::anyhow!("解压后未找到 SMAPI 安装器"))?;

    // 5) 启动安装器（--no-prompt 非交互 + --install + --game-path）。
    //    安装器是控制台程序，会在独立控制台窗口运行；真实环境下可正常完成。
    let dir = installer_exe
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    std::process::Command::new(&installer_exe)
        .current_dir(&dir)
        .args(["--no-prompt", "--install", "--game-path"])
        .arg(game_path)
        .spawn()
        .map_err(|e| anyhow::anyhow!("启动安装器失败：{}", e))?;

    let _ = std::fs::remove_dir_all(&extract);
    Ok("已启动 SMAPI 安装器，稍后可在游戏目录看到 StardewModdingAPI.exe".to_string())
}

/// 解压 zip 到目标目录。
fn extract_zip(zip_path: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    std::fs::create_dir_all(dest)?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let out_path = sanitize_join(dest, entry.name())?;
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            std::fs::write(&out_path, &buf)?;
        }
    }
    Ok(())
}

/// 递归查找 SMAPI 安装器 exe。
fn find_smapi_installer(base: &Path) -> Option<PathBuf> {
    for entry in walkdir::WalkDir::new(base)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() && entry.file_name() == "SMAPI.Installer.exe" {
            return Some(entry.path().to_path_buf());
        }
    }
    None
}

