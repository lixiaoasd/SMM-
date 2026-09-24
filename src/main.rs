#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod bbcode;
mod glass;
mod imgcache;
mod installer;
mod liquid;
mod mirror;
mod model;
mod mods;
mod p2p;
mod paths;
mod server;
mod watch;
mod web;

use std::time::{Duration, Instant};

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // 调试入口：--open-url <url>，不开 GUI，用默认浏览器打开 URL。
    if args.len() >= 3 && args[1] == "--open-url" {
        println!("正在打开：{}", args[2]);
        watch::open_external(&args[2]);
        println!("已发起（详情见 %APPDATA%\\StardewModManager\\watch.log）");
        std::thread::sleep(Duration::from_millis(800));
        return Ok(());
    }

    // 调试入口：--smapi-smoke <游戏目录> <install.dat 路径 或 解压好的安装包目录>
    // 走一遍 SMAPI 覆盖式安装（可以指向假的游戏目录来验证，不会动真实环境）。
    // 需要 debug 构建：release 是 GUI 子系统，看不到输出。
    if args.len() >= 4 && args[1] == "--smapi-smoke" {
        let game = std::path::Path::new(&args[2]);
        let src = std::path::Path::new(&args[3]);
        println!("游戏目录：{}", game.display());
        println!("安装源：{}", src.display());
        let r = if src.is_dir() {
            installer::install_smapi_from_dir(src, game)
        } else {
            installer::install_smapi_dat(src, game)
        };
        match r {
            Ok(m) => println!("结果：OK —— {m}"),
            Err(e) => println!("结果：FAILED —— {e}"),
        }
        return Ok(());
    }

    // 调试入口：--install-smoke <zip 路径> [Mods 目录]
    // 直接对指定压缩包跑一遍「直装」用的 install_archive，验证安装链路
    // （默认装到临时目录，不污染真实 Mods）。需要 debug 构建才能看到输出。
    if args.len() >= 3 && args[1] == "--install-smoke" {
        let zip = std::path::PathBuf::from(&args[2]);
        let mods = match args.get(3) {
            Some(p) => std::path::PathBuf::from(p),
            None => {
                let d = std::env::temp_dir()
                    .join(format!("stardew_install_smoke_{}", std::process::id()))
                    .join("Mods");
                let _ = std::fs::create_dir_all(&d);
                d
            }
        };
        println!("压缩包：{}", zip.display());
        println!("Mods 目录：{}", mods.display());
        match installer::install_archive(&zip, &mods) {
            Ok(names) => println!("结果：OK —— 安装了 {} 个模组：{}", names.len(), names.join("、")),
            Err(e) => println!("结果：FAILED —— {e}"),
        }
        return Ok(());
    }

    // 调试入口：--files-smoke <mod_id>，直接验证 nexus_mod_files + 主文件选择。
    if args.len() >= 3 && args[1] == "--files-smoke" {
        let settings = model::Settings::load();
        let mod_id: u32 = args[2].parse().expect("mod_id 必须是数字");
        println!("api_key 长度={}", settings.nexus_api_key.trim().len());
        match web::nexus_mod_files(settings.nexus_api_key.trim(), mod_id) {
            Ok(files) => {
                println!("解析到 {} 个文件", files.len());
                for f in files.iter().filter(|f| f.category_name == "MAIN").take(5) {
                    println!(
                        "  MAIN file_id={} name={} primary={}",
                        f.file_id, f.name, f.is_primary
                    );
                }
                match web::pick_primary_file(&files) {
                    Some(f) => println!("选中主文件：file_id={} name={}", f.file_id, f.name),
                    None => println!("pick_primary_file 返回 None！"),
                }
            }
            Err(e) => println!("nexus_mod_files 失败：{e}"),
        }
        return Ok(());
    }

    // 调试入口：--watch-smoke，验证浏览器下载监控端到端
    //（自动生成含 manifest.json 的测试 zip 投放到下载文件夹 → 监控发现 → 安装 → 清理）。
    if args.len() >= 2 && args[1] == "--watch-smoke" {
        watch_smoke();
        return Ok(());
    }

    // 调试入口：--mirror-smoke [base_url]，验证镜像源清单→直链下载→进度→安装全链路。
    if args.len() >= 2 && args[1] == "--mirror-smoke" {
        let base = args.get(2).cloned().unwrap_or_else(|| "http://localhost:8770".to_string());
        mirror_smoke(&base);
        return Ok(());
    }

    // 调试入口：--host-smoke，验证一键开服的本地链路
    //（扫存档 → 部署插件 → 写配置 → 发指令 → 读状态 → vnt/端口/防火墙探测）。
    if args.len() >= 2 && args[1] == "--host-smoke" {
        match args.get(2).map(|s| s.as_str()) {
            Some("vnt") => vnt_smoke(),
            Some("p2p") => p2p_smoke(),
            _ => host_smoke(),
        }
        return Ok(());
    }

    // release 下 windows_subsystem=windows 没有控制台，panic 会静默丢失。
    // 安装 panic hook，把崩溃信息写到 %TEMP%\stardew_mod_manager_crash.log 便于排查。
    install_crash_log();

    glass::init(glass::WINDOW_TITLE);

    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([1180.0, 760.0])
        .with_min_inner_size([920.0, 600.0])
        .with_title(glass::WINDOW_TITLE)
        .with_icon(std::sync::Arc::new(egui::IconData {
            rgba: include_bytes!("../assets/icon_256.rgba").to_vec(),
            width: 256,
            height: 256,
        }))
        // 无边框窗口：去掉系统标题栏，改为应用内自绘顶部栏（见 app::ui_title_bar）。
        .with_decorations(false)
        // 透明交换链：DWM 材质（Mica/Acrylic）透过未绘制区域显现。
        .with_transparent(true);

    // 强制 wgpu 使用 GL(OpenGL) 后端，保证本机渲染稳定。
    // SAFETY：单线程启动阶段设置，暂无其他线程读取环境变量。
    unsafe {
        std::env::set_var("WGPU_BACKEND", "opengl");
    }

    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration::default(),
        ..Default::default()
    };

    eframe::run_native(
        "StardewModManager",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}

/// 监控链路冒烟测试。
fn watch_smoke() {
    use std::io::Write as _;

    // 安装到独立目录，不污染真实 Mods。
    let dest_mods = std::env::temp_dir().join(format!("stardew_watch_smoke_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dest_mods);
    watch::set_mods_dir(dest_mods.clone());
    watch::start_session();

    // 构造一个含 manifest.json 的测试 zip，投放到系统下载文件夹。
    let Some(dl) = watch::downloads_dir() else {
        println!("找不到系统下载文件夹");
        return;
    };
    let zip_path = dl.join(format!("stardew_watch_smoke_{}.zip", std::process::id()));
    let f = match std::fs::File::create(&zip_path) {
        Ok(f) => f,
        Err(e) => {
            println!("无法在下载文件夹创建测试 zip：{e}");
            return;
        }
    };
    let mut zw = zip::ZipWriter::new(f);
    let opts = zip::write::FileOptions::default();
    zw.start_file("WatchSmokeMod/manifest.json", opts).unwrap();
    zw.write_all(
        br#"{"Name":"WatchSmokeMod","Version":"1.0.0","UniqueID":"StardewModManager.WatchSmoke","EntryDLL":"dummy.dll"}"#,
    )
    .unwrap();
    zw.start_file("WatchSmokeMod/dummy.dll", opts).unwrap();
    zw.write_all(b"dummy").unwrap();
    zw.finish().unwrap();
    println!("已投放测试 zip：{}", zip_path.display());
    println!("监控目录：{}", dl.display());

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut finished = false;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(700));
        for j in watch::snapshot() {
            println!("[watch] id={} title={} done={} state={:?}", j.id, j.title, j.done, j.state);
        }
        if watch::snapshot()
            .iter()
            .any(|j| matches!(j.state, watch::WState::Finished(..)))
        {
            finished = true;
            break;
        }
    }

    let installed = dest_mods.join("WatchSmokeMod").is_dir();
    let zip_gone = !zip_path.exists();
    println!("监控结束：完成={finished}");
    println!("安装目录存在 WatchSmokeMod：{installed}");
    println!("测试 zip 已被清理：{zip_gone}");
    watch::stop_session();
}

/// P2P 联机大厅冒烟测试：创建档案 → 启动 HTTP 服务 → 验证 /profile 和 /mods → 停止。
fn p2p_smoke() {
    let settings = model::Settings::load();
    let mods_path = paths::detect(settings.game_path.as_deref())
        .mods_path
        .unwrap_or_default();

    // 创建测试档案
    let profile = p2p::Profile {
        uid: p2p::generate_uid(),
        name: "SmokeTest".to_string(),
        avatar: "🌾".to_string(),
    };
    println!("档案：{} {}（UID={}）", profile.avatar, profile.name, profile.uid);

    // 启动 P2P 服务
    match p2p::start_server(mods_path.clone(), profile.clone()) {
        Ok(()) => println!("P2P 服务已启动（端口 {}）", p2p::P2P_PORT),
        Err(e) => {
            println!("启动失败：{e}");
            return;
        }
    }

    std::thread::sleep(Duration::from_secs(1));

    // 测试 /profile
    let url = format!("http://127.0.0.1:{}/profile", p2p::P2P_PORT);
    println!("GET {url}");
    match reqwest::blocking::get(&url) {
        Ok(resp) => {
            println!("  status = {}", resp.status());
            if resp.status().is_success() {
                let p: p2p::Profile = resp.json().unwrap_or_default();
                println!("  profile = {} {} (UID={})", p.avatar, p.name, p.uid);
            }
        }
        Err(e) => println!("  请求失败：{e}"),
    }

    // 测试 /mods
    let mut first_folder: Option<String> = None;
    let url = format!("http://127.0.0.1:{}/mods", p2p::P2P_PORT);
    println!("GET {url}");
    match reqwest::blocking::get(&url) {
        Ok(resp) => {
            println!("  status = {}", resp.status());
            if resp.status().is_success() {
                let mods: Vec<p2p::SharedMod> = resp.json().unwrap_or_default();
                println!("  共享模组数 = {}", mods.len());
                for m in mods.iter().take(5) {
                    println!("    {} v{} by {} ({})", m.name, m.version, m.author, m.size_label());
                }
                first_folder = mods.first().map(|m| m.folder.clone());
            }
        }
        Err(e) => println!("  请求失败：{e}"),
    }

    // ── 路径穿越回归测试 ──
    // 用百分号编码绕开客户端对点段的归一化，直接考验服务端解码后的校验
    //（issue #3：/mods/ 曾可读任意目录；注意 Windows 上 `\Windows`、`C:foo`
    //  的 is_absolute() 都是 false，只查绝对路径和 .. 是不够的）。
    println!("\n路径穿越回归测试（全部应为 404）：");
    let attacks = [
        "/mods/%2e%2e%2f%2e%2e%2fWindows",
        "/mods/..%5c..%5cWindows",
        "/mods/%5cWindows",
        "/mods/%2fWindows",
        "/mods/C%3afoo",
        "/mods/%5c%5cserver%5cshare",
    ];
    let mut leaked = 0;
    for a in attacks {
        let full = format!("http://127.0.0.1:{}{a}", p2p::P2P_PORT);
        match reqwest::blocking::get(&full) {
            Ok(resp) => {
                let code = resp.status().as_u16();
                let bad = code == 200;
                if bad {
                    leaked += 1;
                }
                println!(
                    "  {:<34} -> {}{}",
                    a,
                    code,
                    if bad { "   ← 越界读到了东西！" } else { "" }
                );
            }
            Err(e) => println!("  {a} 请求失败：{e}"),
        }
    }
    println!("  越界用例数 = {leaked}（必须为 0）");

    // 正常路径仍应可用
    if let Some(folder) = first_folder {
        let full = format!("http://127.0.0.1:{}/mods/{}", p2p::P2P_PORT, folder);
        match reqwest::blocking::get(&full) {
            Ok(resp) => println!(
                "\n正常下载 /mods/{} -> {}（{} 字节，应为 200）",
                folder,
                resp.status(),
                resp.bytes().map(|b| b.len()).unwrap_or(0)
            ),
            Err(e) => println!("\n正常下载请求失败：{e}"),
        }
    }

    // 测试附近玩家（本地只有自己，应该 0 个）
    let peers = p2p::get_peers();
    println!("附近玩家数 = {}（本机自身不计）", peers.len());

    // 停止
    p2p::stop_server();
    println!("P2P 服务已停止");
}

/// vnt 下载链路冒烟测试：从 GitHub 拉最新 release → 解压到 %APPDATA%\StardewModManager\vnt。
fn vnt_smoke() {
    println!("vnt 目录：{}", server::vnt_dir().display());
    println!("下载前已安装：{}", server::vnt_installed());
    match server::vnt_download() {
        Ok(p) => println!("下载并解压完成：{}", p.display()),
        Err(e) => {
            println!("vnt 下载失败：{e}");
            return;
        }
    }
    println!("下载后已安装：{}", server::vnt_installed());
    match server::vnt_exe() {
        Some(p) => println!("可执行文件：{}", p.display()),
        None => println!("没找到 vnt 可执行文件！"),
    }
    let rt = server::vnt_probe();
    println!("运行中={} 虚拟 IP={:?} 网卡={:?}", rt.running, rt.ip, rt.iface);
}

/// 一键开服链路冒烟测试（不启动游戏，只验证管理器这一侧的读写与探测）。
fn host_smoke() {
    let settings = model::Settings::load();
    let Some(game) = paths::detect_game_path(settings.game_path.as_deref()) else {
        println!("未能定位游戏目录，请先在设置页指定");
        return;
    };
    println!("游戏目录：{}", game.display());

    println!("--- 存档扫描 ---");
    let saves = server::scan_saves();
    println!("解析到 {} 个存档", saves.len());
    for s in saves.iter().take(5) {
        println!(
            "  {:<28} {:<18} {} 可开房={}",
            s.folder,
            s.title(),
            s.saved_ago_label(),
            s.can_host
        );
    }
    let hostable = saves.iter().find(|s| s.can_host);
    match hostable {
        Some(s) => println!("默认选中：{}（{}）", s.folder, s.title()),
        None => println!("没有可开房存档"),
    }

    println!("--- 插件部署 ---");
    match server::deploy_hostkit(&game) {
        Ok(()) => println!("已部署到：{}", server::hostkit_dir(&game).display()),
        Err(e) => println!("部署失败：{e}"),
    }

    let mut cfg = settings.host.clone();
    // 冒烟测试不设 auto_host=true，避免下次启动游戏时自动开房。
    cfg.auto_host = false;
    cfg.save_name = hostable.map(|s| s.folder.clone()).unwrap_or_default();
    match server::write_config(&game, &cfg) {
        Ok(()) => println!("配置已写入（存档={}，AFK={} 分钟）", cfg.save_name, cfg.afk_minutes),
        Err(e) => println!("写配置失败：{e}"),
    }

    println!("--- 状态回读 ---");
    match server::read_status(&game) {
        Some(s) => println!(
            "开服中={} 时间暂停={}({}) 玩家={} 时钟={} {}",
            s.status.hosting,
            s.status.time_paused,
            s.status.pause_reason(),
            s.status.players,
            s.status.clock(),
            s.status.season_label()
        ),
        None => println!("暂无状态文件（游戏未运行时会这样，属正常）"),
    }

    println!("--- 环境探测 ---");
    println!("游戏进程运行中：{}", server::game_running());
    println!("联机端口 {} 已占用：{}", server::GAME_PORT, server::port_in_use(server::GAME_PORT));
    println!("防火墙已放行：{}", server::firewall_allowed(server::GAME_PORT));
    match server::primary_local_ip() {
        Some(ip) => println!("本机局域网地址：{ip}"),
        None => println!("未取到本机局域网地址"),
    }
    let rt = server::vnt_probe();
    println!(
        "vnt 已安装={} 运行中={} 虚拟 IP={:?} 网卡={:?}",
        server::vnt_installed(),
        rt.running,
        rt.ip,
        rt.iface
    );
    let log = server::vnt_log_tail(5);
    if !log.trim().is_empty() {
        println!("vnt 日志尾部：\n{log}");
    }
}

/// 镜像源链路冒烟测试：拉清单 → 下载第一个模组 → 安装到临时 Mods 目录。
fn mirror_smoke(base: &str) {
    let dest_mods = std::env::temp_dir().join(format!("stardew_mirror_smoke_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dest_mods);
    mirror::set_mods_dir(dest_mods.clone());

    println!("镜像源：{base}");
    let list = match mirror::fetch_index(base) {
        Ok(l) => l,
        Err(e) => {
            println!("拉取清单失败：{e}（服务器启动了吗？mirror-server\\start-server.ps1）");
            return;
        }
    };
    println!("清单含 {} 个模组", list.len());
    if list.is_empty() {
        println!("清单为空，先往 mirror-server\\public\\mods 放 zip 并跑 rebuild-index.ps1");
        return;
    }
    for mm in &list {
        println!("开始下载：{}（{}，{} bytes）", mm.name, mm.file, mm.size);
        mirror::download(base.to_string(), mm.clone());
    }

    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let jobs = mirror::snapshot();
        for j in &jobs {
            println!(
                "[mirror] {:<28} {:?} {}/{} {}",
                j.title,
                j.state,
                j.done,
                j.total,
                match &j.state {
                    mirror::MState::Finished(_, d) => d,
                    _ => "",
                }
            );
        }
        let all_done = jobs.len() == list.len() && jobs.iter().all(|j| !j.is_active());
        if all_done || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let scanned = mods::scan_mods(&dest_mods);
    println!("临时 Mods 目录模组数：{}", scanned.len());
    for m in &scanned {
        println!("  - {} v{} by {}", m.name(), m.version(), m.author());
    }
}

/// 把 panic 信息写入 %TEMP%\stardew_mod_manager_crash.log（GUI 子系统下无控制台输出）。
fn install_crash_log() {
    std::panic::set_hook(Box::new(|info| {
        use std::io::Write;
        let path = std::env::temp_dir().join("stardew_mod_manager_crash.log");
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "=== {} ===", chrono_like_timestamp());
            let _ = writeln!(f, "{info}");
            let _ = f.flush();
        }
    }));
}

fn chrono_like_timestamp() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix {d}")
}
