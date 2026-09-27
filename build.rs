//! 构建脚本（星露谷物语模组管理器）。
//!
//! - Windows 下用 windres 把 assets/app.ico 编译为 COFF 目标文件并链接进 exe，
//!   使文件资源管理器 / 任务栏显示自定义图标。
//! - 同时生成并编译版本信息资源（VERSIONINFO）：版本号直接取自 CARGO_PKG_VERSION，
//!   不会和 Cargo.toml 脱节；公司名/产品名/文件说明是系统与杀软信誉评估会看的元数据。
//!
//! 本机没有 gcc/cc1，windres 的 C 预处理阶段无法运行；而我们的 .rc 只有资源定义、
//! 不使用任何宏，因此用系统自带的 csc.exe 编译一个"透传预处理器"
//! （原样把 .rc 内容输出到 stdout），通过 --preprocessor 交给 windres。

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let rc = manifest.join("assets/app.rc");
    let ico = manifest.join("assets/app.ico");

    if !rc.exists() || !ico.exists() {
        println!("cargo:warning=缺少图标资源（assets/app.rc 或 app.ico），跳过资源嵌入");
        return;
    }

    let pass_exe = build_passthrough_preprocessor(&out_dir);
    let windres = find_windres();

    // ---------- 图标 ----------
    let icon_obj = out_dir.join("app_icon.o");
    compile_rc(&windres, &pass_exe, &manifest, &rc, &icon_obj);
    println!("cargo:rustc-link-arg={}", icon_obj.display());

    // ---------- 版本信息（版本号来自 Cargo.toml） ----------
    let version_rc = out_dir.join("version.rc");
    std::fs::write(&version_rc, version_rc_source()).unwrap();
    let version_obj = out_dir.join("version_info.o");
    compile_rc(&windres, &pass_exe, &manifest, &version_rc, &version_obj);
    println!("cargo:rustc-link-arg={}", version_obj.display());

    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-env-changed=CARGO_PKG_VERSION");
}

/// 版本信息资源的 `.rc` 内容。
///
/// 用 0x0409（en-US）+ 1200（UTF-16）这一组最常见的组合，值本身可以是中文：
/// Windows 资源模型里由 code page 决定编码，语言标记只影响排序/回退。
fn version_rc_source() -> String {
    let ver = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    let mut parts = ver.split('.').map(|p| p.parse::<u16>().unwrap_or(0));
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    let patch = parts.next().unwrap_or(0);

    format!(
        "1 VERSIONINFO\n\
         FILEVERSION {major},{minor},{patch},0\n\
         PRODUCTVERSION {major},{minor},{patch},0\n\
         FILEFLAGSMASK 0x3fL\n\
         FILEFLAGS 0x0L\n\
         FILEOS 0x40004L\n\
         FILETYPE 0x1L\n\
         FILESUBTYPE 0x0L\n\
         BEGIN\n  \
         BLOCK \"StringFileInfo\"\n  \
         BEGIN\n    \
         BLOCK \"040904b0\"\n    \
         BEGIN\n      \
         VALUE \"CompanyName\", \"FireSVM\"\n      \
         VALUE \"FileDescription\", \"星露谷物语模组管理器\"\n      \
         VALUE \"FileVersion\", \"{ver}\"\n      \
         VALUE \"InternalName\", \"stardew-mod-manager\"\n      \
         VALUE \"LegalCopyright\", \"MIT License\"\n      \
         VALUE \"OriginalFilename\", \"stardew-mod-manager.exe\"\n      \
         VALUE \"ProductName\", \"Stardew Valley Mod Manager\"\n      \
         VALUE \"ProductVersion\", \"{ver}\"\n    \
         END\n  \
         END\n  \
         BLOCK \"VarFileInfo\"\n  \
         BEGIN\n    \
         VALUE \"Translation\", 0x409, 1200\n  \
         END\n\
         END\n"
    )
}

/// 用 csc 编译一个"透传预处理器"（原样输出 .rc 内容），返回它的路径。
fn build_passthrough_preprocessor(out_dir: &Path) -> PathBuf {
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

    let csc_status = Command::new(csc)
        .arg("-nologo")
        .arg(format!("-out:{}", pass_exe.display()))
        .arg(&pass_cs)
        .status()
        .expect("启动 csc 失败");
    assert!(csc_status.success(), "csc 编译 rcpass.exe 失败");

    pass_exe
}

/// 找一个可用的 windres。
fn find_windres() -> PathBuf {
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME"));
    let mut candidates = vec!["windres".to_string(), "windres.exe".to_string()];
    if let Ok(h) = &home {
        candidates.push(format!("{h}\\mingw64\\bin\\windres.exe"));
    }
    let found = candidates
        .into_iter()
        .find(|c| {
            if c.contains('\\') || c.contains('/') {
                Path::new(c).exists()
            } else {
                std::env::var_os("PATH")
                    .map(|p| std::env::split_paths(&p).any(|d| d.join(c).exists()))
                    .unwrap_or(false)
            }
        })
        .expect("找不到 windres.exe（应在 ~/mingw64/bin 下）");
    PathBuf::from(found)
}

/// windres 编译 `.rc` → COFF 目标文件。
///
/// 固定 `--codepage=65001`：`.rc` 一律按 UTF-8 保存（里面有中文版本说明），
/// 不指定时 windres 会按默认代码页解释，中文会变成乱码。
fn compile_rc(windres: &Path, pass_exe: &Path, manifest: &Path, rc: &Path, obj: &Path) {
    let status = Command::new(windres)
        .current_dir(manifest)
        .arg(format!("--preprocessor={}", pass_exe.display()))
        .arg("--codepage=65001")
        .arg(rc)
        .arg("-O")
        .arg("coff")
        .arg("-o")
        .arg(obj)
        .status()
        .expect("启动 windres 失败");
    assert!(status.success(), "windres 编译资源失败：{}", rc.display());
}