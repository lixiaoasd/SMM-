#!/usr/bin/env bash
# 星露谷物语模组管理器 构建脚本
# 环境说明：本机无 MSVC / MinGW gcc，使用 Rust GNU 工具链自包含链接，
# 并补充 mingw-w64 binutils（as/ld/dlltool 等）与导入库（lib*.a）。
#
# 前置（本机已就绪）：
#   1) Rust GNU 工具链：~/.rustup/toolchains/stable-x86_64-pc-windows-gnu
#   2) binutils：        ~/mingw64/bin（as.exe / dlltool.exe / ld.exe ...）
#   3) 导入库：          ~/mingw64/lib（libkernel32.a / libshlwapi.a ...）
#                      已合并进工具链 self-contained 目录
set -e

TC="$HOME/.rustup/toolchains/stable-x86_64-pc-windows-gnu"
export PATH="$HOME/mingw64/bin:$TC/bin:$TC/lib/rustlib/x86_64-pc-windows-gnu/bin/self-contained:$PATH"
export RUSTUP_DIST_SERVER=https://rsproxy.cn

cd "$(dirname "$0")"

# ---------- 1) 房主助手插件：用 Windows 自带 csc 编译 ----------
# 本机没有 .NET SDK，改用 %WINDIR%\Microsoft.NET\Framework64 下的 csc.exe，
# 并用 /nostdlib+ 引用游戏目录里自带的 net6 程序集。
# 注意：csc 只认 C# 5 语法（不能用 $"..."、?.、out var、表达式体成员）。
GAME_DIR="${STARDEW_GAME:-C:/Program Files (x86)/Steam/steamapps/common/Stardew Valley}"
CSC="C:/Windows/Microsoft.NET/Framework64/v4.0.30319/csc.exe"
HOSTKIT_SRC="stardew-hostkit/src/ModEntry.cs"
HOSTKIT_OUT="stardew-hostkit/bin/FireSVM.HostKit.dll"

if [ -f "$HOSTKIT_SRC" ]; then
    if [ ! -f "$HOSTKIT_OUT" ] || [ "$HOSTKIT_SRC" -nt "$HOSTKIT_OUT" ]; then
        if [ ! -f "$CSC" ]; then
            echo "找不到 csc.exe（$CSC），跳过插件编译"
        elif [ ! -d "$GAME_DIR" ]; then
            echo "找不到游戏目录（$GAME_DIR），跳过插件编译；可用 STARDEW_GAME=... 指定"
        else
            echo "编译房主助手插件 → $HOSTKIT_OUT"
            mkdir -p stardew-hostkit/bin
            "$CSC" -nologo -codepage:65001 -nostdlib+ -target:library \
                -out:"$HOSTKIT_OUT" \
                -reference:"$GAME_DIR/System.Private.CoreLib.dll" \
                -reference:"$GAME_DIR/System.Runtime.dll" \
                -reference:"$GAME_DIR/System.Collections.dll" \
                -reference:"$GAME_DIR/Stardew Valley.dll" \
                -reference:"$GAME_DIR/StardewModdingAPI.dll" \
                "$HOSTKIT_SRC"
        fi
    fi
fi
if [ ! -f "$HOSTKIT_OUT" ]; then
    echo "缺少 $HOSTKIT_OUT —— 它会被内嵌进管理器，无法继续编译" >&2
    exit 1
fi

# ---------- 2) 管理器本体 ----------
MODE="${1:-release}"
case "$MODE" in
    release)
        "$TC/bin/cargo.exe" build --release -j 4
        echo "完成：target/release/stardew-mod-manager.exe"
        ;;
    locked)
        "$TC/bin/cargo.exe" build --release --features locked-mirror -j 4
        echo "完成：target/release/stardew-mod-manager.exe（locked-mirror）"
        ;;
    *)
        "$TC/bin/cargo.exe" build -j 4
        echo "完成：target/debug/stardew-mod-manager.exe"
        ;;
esac
