//! 构建脚本（星露谷物语模组管理器）。
//!
//! - WebView2Loader.dll 已通过 vendor 的 webview2-com-sys 用 `include_bytes!`
//!   嵌入可执行文件，运行时动态加载，无需复制任何 DLL。
//! - Windows 下用 windres 把 assets/app.ico 编译为 COFF 目标文件并链接进 exe，
//!   使文件资源管理器 / 任务栏显示自定义图标。
//!
//! 本机没有 gcc/cc1，windres 的 C 预处理阶段无法运行；而我们的 .rc 只有一行
//! 资源定义、不使用任何宏，因此用系统自带的 csc.exe 编译一个"透传预处理器"
//! （原样把 .rc 内容输出到 stdout），通过 --preprocessor 交给 windres。

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let rc = manifest.join("assets/app.rc");
    let ico = manifest.join("assets/app.ico");
    let obj = out_dir.join("app_icon.o");

    if !rc.exists() || !ico.exists() {
        println!("cargo:warning=缺少图标资源（assets/app.rc 或 app.ico），跳过 exe 图标嵌入");
        return;
    }

    // ---------- 1) 编译透传预处理器 rcpass.exe ----------
    let pass_cs = out_dir.join("rcpass.cs");
    let pass_exe = out_dir.join("rcpass.exe");
    std::fs::write(
        &pass_cs,
        "using System; using System.IO;\n\
         class P {\n  \
         static int Main(string[] a) {\n    \
         string file = null;\n    \
         foreach (string s in a) { string x = s.Trim('\"'); if (File.Exists(x)) file = x; }\n    \
         Stream src = (file != null) ? (Stream)File.OpenRead(file) : Console.OpenStandardInput();\n    \
         using (src) { src.CopyTo(Console.OpenStandardOutput()); }\n    \
         return 0;\n  }\n}\n",
    )
    .unwrap();

    let csc = [
        r"C:\Windows\Microsoft.NET\Framework64\v4.0.30319\csc.exe",
        r"C:\Windows\Microsoft.NET\Framework\v4.0.30319\csc.exe",
    ]
    .into_iter()
    .find(|p| std::path::Path::new(p).exists())
    .expect("找不到 .NET Framework csc.exe");

    let csc_status = std::process::Command::new(csc)
        .arg("-nologo")
        .arg(format!("-out:{}", pass_exe.display()))
        .arg(&pass_cs)
        .status()
        .expect("启动 csc 失败");
    assert!(csc_status.success(), "csc 编译 rcpass.exe 失败");

    // ---------- 2) 找 windres ----------
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME"));
    let mut candidates = vec!["windres".to_string(), "windres.exe".to_string()];
    if let Ok(h) = &home {
        candidates.push(format!("{h}\\mingw64\\bin\\windres.exe"));
    }
    let windres = candidates
        .into_iter()
        .find(|c| {
            if c.contains('\\') || c.contains('/') {
                std::path::Path::new(c).exists()
            } else {
                std::env::var_os("PATH")
                    .map(|p| std::env::split_paths(&p).any(|d| d.join(c).exists()))
                    .unwrap_or(false)
            }
        })
        .expect("找不到 windres.exe（应在 ~/mingw64/bin 下）");

    // ---------- 3) windres 编译 rc -> coff ----------
    let status = std::process::Command::new(&windres)
        .current_dir(&manifest)
        .arg(format!("--preprocessor={}", pass_exe.display()))
        .arg(&rc)
        .arg("-O")
        .arg("coff")
        .arg("-o")
        .arg(&obj)
        .status()
        .expect("启动 windres 失败");
    assert!(status.success(), "windres 编译图标资源失败");

    println!("cargo:rustc-link-arg={}", obj.display());
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/app.ico");
}
