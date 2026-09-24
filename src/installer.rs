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
        // 有的压缩包是「SMAPI 安装器」而不是普通模组（镜像站收录的 SMAPI 条目就是
        // 官方 SMAPI-x.y.z-installer.zip）：它没有 manifest.json，但带
        // internal/windows/install.dat。按普通模组装必然失败，这里转到 SMAPI 安装流程。
        if let Some(dat) = find_smapi_dat(&tmp) {
            let game = mods_dir
                .parent()
                .map(|p| p.to_path_buf())
                .filter(|g| looks_like_game_dir(g));
            let r = match game {
                Some(g) => install_smapi_dat(&dat, &g).map(|_| vec!["SMAPI".to_string()]),
                None => Err(anyhow::anyhow!(
                    "这是 SMAPI 安装包而不是普通模组，且 Mods 目录不在游戏目录下，\
                     无法定位游戏目录；请改用「设置 → 一键安装/更新 SMAPI」"
                )),
            };
            let _ = std::fs::remove_dir_all(&tmp);
            return r;
        }
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

/// 从内存中的 zip 数据安装（P2P 补齐 / 镜像补齐用）。
///
/// 先落一个独占临时文件再走 `install_archive` 的统一流程，好处是复用
/// 全局安装锁与 zip slip 校验——直接 `extract_zip` 到 Mods 会绕过安装锁，
/// 与镜像直装/浏览器监控并发时互相覆盖。
pub fn install_zip_bytes(data: &[u8], mods_dir: &Path) -> Result<Vec<String>> {
    let seq = EXTRACT_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!(
        "stardew_p2p_{}_{}.zip",
        std::process::id(),
        seq
    ));
    std::fs::write(&tmp, data)?;
    let r = install_archive(&tmp, mods_dir);
    let _ = std::fs::remove_file(&tmp);
    r
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

/// 一键安装 / 更新 SMAPI：下载官方安装包 → 解压 → 按 install.dat 覆盖式安装。
///
/// 不再调用官方 SMAPI.Installer.exe：它只认 `--install` / `--uninstall`，根本没有
/// `--game-path` / `--no-prompt`；而且是交互式控制台程序（启动就调 Console.Clear()），
/// 从 GUI 进程以无控制台方式 spawn 会直接抛 IOException（句柄无效）失败，
/// 等它读输入还会把界面永久挂住。官方三个启动脚本也都是无参数调用，没有静默接口。
pub fn install_smapi(game_path: &Path) -> Result<String> {
    // 1) 从 GitHub 获取最新版下载地址。
    let url = crate::web::smapi_latest_release_url()
        .ok_or_else(|| anyhow::anyhow!("无法获取 SMAPI 下载地址（检查网络后重试）"))?;

    // 2) 下载安装包。
    let zip_path = crate::web::download_smapi(&url)
        .map_err(|e| anyhow::anyhow!("下载失败：{}", e))?;

    // 3) 解压到临时目录，取出 install.dat 并安装。
    let extract = std::env::temp_dir().join(format!("stardew_smapi_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&extract);
    let result = (|| -> Result<String> {
        extract_zip(&zip_path, &extract)?;
        install_smapi_from_dir(&extract, game_path)
    })();

    let _ = std::fs::remove_dir_all(&extract);
    if result.is_ok() {
        // 装好就不再留这份 40MB 的临时包；失败时保留，方便排查。
        let _ = std::fs::remove_file(&zip_path);
    }
    result
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

/// 从解压好的 SMAPI 安装包目录里找出 install.dat 并安装。
pub fn install_smapi_from_dir(dir: &Path, game_path: &Path) -> Result<String> {
    let dat =
        find_smapi_dat(dir).ok_or_else(|| anyhow::anyhow!("安装包内未找到 install.dat"))?;
    install_smapi_dat(&dat, game_path)
}

/// 在解压出的安装包里找 install.dat —— 官方包里 windows/linux/macOS 各有一份，
/// Windows 版优先。
fn find_smapi_dat(base: &Path) -> Option<PathBuf> {
    let mut fallback: Option<PathBuf> = None;
    for entry in walkdir::WalkDir::new(base)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() || entry.file_name() != "install.dat" {
            continue;
        }
        let path = entry.path().to_path_buf();
        let is_windows = path
            .parent()
            .and_then(|d| d.file_name())
            .map(|n| n.eq_ignore_ascii_case("windows"))
            .unwrap_or(false);
        if is_windows {
            return Some(path);
        }
        fallback.get_or_insert(path);
    }
    fallback
}

/// 目录是否像星露谷游戏目录（至少要有一个游戏本体文件）。
fn looks_like_game_dir(game: &Path) -> bool {
    game.join("Stardew Valley.exe").is_file()
        || game.join("StardewValley.exe").is_file()
        || game.join("Stardew Valley.deps.json").is_file()
}

/// 把 SMAPI 的 install.dat（其实是改了扩展名的 zip）安装进游戏目录。
///
/// 复刻官方安装包 README.txt 的 manual install 流程：
///   ① 解压 install.dat 覆盖到游戏目录；但 `Mods/` 下已存在的内置模组
///      （ConsoleCommands / SaveBackup，含用户改名成 `.X` 的禁用形态）跳过，
///      否则会和用户那份重名成两个模组；
///   ② 保留用户已有的 smapi-internal/config.json（SMAPI 启动会自己补默认值）；
///   ③ 把游戏目录的 `Stardew Valley.deps.json` 复制成 `StardewModdingAPI.deps.json`。
pub fn install_smapi_dat(dat: &Path, game_path: &Path) -> Result<String> {
    if !looks_like_game_dir(game_path) {
        anyhow::bail!(
            "{} 不像星露谷游戏目录（找不到 Stardew Valley.exe / Stardew Valley.deps.json）",
            game_path.display()
        );
    }

    let cfg = game_path.join("smapi-internal").join("config.json");
    let saved_cfg = std::fs::read(&cfg).ok();

    // 安装前先记下 Mods 里已有的模组文件夹（含 `.X` 禁用形态）：install.dat 里带
    // SMAPI 内置模组（ConsoleCommands / SaveBackup），用户已有同名目录时不能覆盖，
    // 否则会和用户那份重名成两个模组。
    //
    // 必须在循环外一次性快照：zip 的目录条目会先把 `Mods/SaveBackup/` 建出来，
    // 若在循环里逐条判断「是否已存在」，紧随其后的文件条目就会被自己刚建的目录判成
    // 「已存在」而整包跳过。
    let mut existing_mods: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Ok(rd) = std::fs::read_dir(game_path.join("Mods")) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            existing_mods.insert(n.trim_start_matches('.').to_ascii_lowercase());
        }
    }

    let file = std::fs::File::open(dat)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut written = 0usize;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry.name().replace('\\', "/");
        if let Some(rest) = name.strip_prefix("Mods/") {
            let top = rest.split('/').next().unwrap_or("");
            if !top.is_empty() && existing_mods.contains(&top.to_ascii_lowercase()) {
                continue;
            }
        }
        let out_path = sanitize_join(game_path, &name)?;
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            std::fs::write(&out_path, &buf)?;
            written += 1;
        }
    }

    if let Some(bytes) = saved_cfg {
        std::fs::write(&cfg, bytes)?;
    }

    let game_deps = game_path.join("Stardew Valley.deps.json");
    if game_deps.is_file() {
        std::fs::copy(&game_deps, game_path.join("StardewModdingAPI.deps.json"))?;
    }

    std::fs::create_dir_all(game_path.join("Mods"))?;

    if !game_path.join("StardewModdingAPI.exe").is_file() {
        anyhow::bail!("安装后没看到 StardewModdingAPI.exe，安装包可能不完整");
    }
    Ok(format!("已写入游戏目录 {written} 个文件"))
}

