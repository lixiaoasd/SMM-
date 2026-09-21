# FireSVM — 星露谷物语模组管理器

Windows 桌面端的 Stardew Valley / SMAPI 模组管理器，Rust + egui 编写，白色液态玻璃界面。
包含三部分：

| 组件 | 目录 | 说明 |
| --- | --- | --- |
| 桌面管理器 | `src/` | egui 程序（模组管理 / 镜像库 / 内嵌浏览器 / 联机） |
| 游戏内插件 | `stardew-hostkit/` | SMAPI C# 插件，与管理器通过文件通信 |
| 镜像服务端 | `mirror-server/` | Nexus Mods 同步脚本 + nginx 配置（Python/PowerShell） |

## 功能

**本地模组**

- 扫描 `Mods/`：名称 / 版本 / 作者 / 标签 / 启用状态，搜索与过滤
- 启用 / 禁用（SMAPI 的 `.` 前缀约定）、批量操作、一键反转
- 自定义标签与备注、模组组合（Profile）一键切换
- 从 zip 安装：自动展开 N 网常见的外层版本号目录；独占临时目录 + 全局安装锁
- 仅含 `i18n/*.json` 的汉化包自动合并进已安装的主模组
- SMAPI 管理：Steam 库 / 游戏路径检测、启动游戏、下载 SMAPI
- 红字终结者：解析 SMAPI 日志报错并给出常见解释
- AI 翻译（OpenAI 兼容接口）：扫描 i18n 缺失键并一键补全
- GitHub 更新检查（manifest 的 `GitHub:` UpdateKey）

**镜像模组库（默认镜像 http://116.62.231.162，可在设置页修改）**

- 镜像索引含中文译名 / 简介 / BBCode 长描述中文机翻 / 封面图 / 分类 / 合集
- 封面与描述内图片异步加载，内存 + 本地磁盘两级缓存（`imgcache.rs`）
- 前置依赖自动检测：缺前置橙色警告，可「⚡ 一并安装」（前置先入队）
- 冲突双向检测（模组自身规则 + 已装模组规则，大小写不敏感正则）
- 镜像直装走 Nexus API 元数据 + 镜像 CDN 下载，绕过 Cloudflare

**内嵌浏览器与下载**

- 四个 WebView2 浏览器共享 cookie 与 `cf_clearance`（使用系统默认 UA）
- 串行下载队列，一次只打开一个文件页；支持「跳过此模组」「停止队列」
- 监控「下载」文件夹识别浏览器下载完成
- 灵动岛下载状态组件（空闲 / 下载 / 完成 / 悬停展开详情）

**联机**

- 局域网 UDP 自动发现 + TCP 模组传输
- VNT 内网穿透（默认公共服务端，可自建），加入朋友服务器时自动检测缺失模组，
  弹窗列出全部所需下载，确认后自动补齐
- 游戏内 `FireSVM.HostKit` 插件经 `Mods/FireSVM.HostKit/` 下
  `hostkit-config.json` / `hostkit-status.json` / `hostkit-cmd.txt` 三个文件与管理器通信（无网络协议）

## 目录结构

```
src/
  main.rs        入口、命令行 smoke 调试
  app.rs         界面与全部交互状态
  model.rs       数据模型、设置持久化（%APPDATA%\StardewModManager\settings.json）
  paths.rs       Steam / 星露谷 / SMAPI 路径检测、VDF 解析
  mods.rs        模组扫描、启停、标签、组合
  installer.rs   zip 安装、i18n 覆盖安装、GitHub 更新检查
  web.rs         Nexus API、SMAPI 下载、AI 翻译、i18n 缺失分析
  mirror.rs      镜像索引模型、过滤/排序/警告的 MirrorView 缓存
  imgcache.rs    图片异步加载（内存 LRU + 磁盘缓存 + 缩略）
  bbcode.rs      Nexus 描述 BBCode 渲染（含外链图片）
  p2p.rs         局域网发现与模组传输
  server.rs      联机服务端、HostKit 部署、VNT 配置
  watch.rs       下载文件夹监控
  glass.rs       Win32 无边框玻璃窗口（去标题栏/字幕风格控制）
  liquid.rs      液态玻璃主题组件
stardew-hostkit/
  src/ModEntry.cs   插件源码（C# 5）
  manifest.json     SMAPI manifest
  bin/              预编译 dll（编译期 include_bytes! 内嵌，必须保留）
mirror-server/
  sync-nexus.py        服务器同步（元数据/zip/封面/火山翻译，断点续传）
  backfill-extras.py   一次性幂等回填：封面 + 描述 + 中文翻译
  sync-nexus.ps1 等    Windows 侧早期同步/维护脚本
  nginx-mirror.conf    nginx 站点配置
  mirror-sync.service/.timer  systemd 定时同步（每日 04:00）
assets/           图标；icon_256.rgba 为编译期内嵌资源
installer/        Inno Setup 安装脚本
tmp_dll/iconv_stub/  windres 依赖的 libiconv-2.dll 桩库源码（见「构建说明」）
```

## 构建（Windows）

环境：Rust **stable-x86_64-pc-windows-gnu** 工具链（自包含链接，无需 MSVC/gcc），
另需 mingw-w64 的 binutils（`windres.exe` 等，放在 `%USERPROFILE%\mingw64\bin`）。

```bash
./build.sh           # 先编译 HostKit 插件（需要本机装有星露谷+SMAPI），再 cargo build --release
cargo build --release
```

- crates.io 走 `rsproxy.cn` 镜像（见 `.cargo/config.toml`）
- `stardew-hostkit/bin/FireSVM.HostKit.dll` 已随仓库提供预编译版本；
  如需从源码重编，`build.sh` 会调用游戏目录 net6 程序集用系统自带 csc 编译
  （插件源码限定 **C# 5** 语法）
- `build.rs` 在编译期用 windres 把 `assets/app.ico` 链入 exe；windres 需要
  libiconv 等 4 个 DLL（其中 libiconv-2.dll 可用 `tmp_dll/iconv_stub/` 造桩）
- 调试入口统一走 `--xxx-smoke` 命令行参数（release 无控制台）

安装包：用 Inno Setup 编译 `installer/setup.iss`。

## 镜像服务端部署

1. nginx 站点根目录（默认 `/var/www/mirror`），配置见 `mirror-server/nginx-mirror.conf`
2. 脚本放 `/opt/mirror/`，自备两个密钥文件（**不随仓库分发**）：
   - `nexus-api.key`：Nexus Mods API key（需 premium 账号）
   - `volc-mt.key`：火山引擎机器翻译 key（不需要中文翻译可不留，翻译自动降级为空）
3. 安装依赖后启用 systemd timer：

   ```bash
   pip3 install requests
   cp mirror-sync.service mirror-sync.timer /etc/systemd/system/
   systemctl daemon-reload && systemctl enable --now mirror-sync.timer
   ```

   定时任务**必须直连** Nexus，不要给 ExecStart 加 `--proxy`；
   `run-sync.sh` 仅作为需要代理时的手动备用。
4. 存量数据回填：`python3 backfill-extras.py`（阶段 A 封面/描述，阶段 B 翻译，
   幂等可中断重跑；`--only A|B`、`--limit N` 冒烟）

## 隐私说明

- N 网账号密码与 API key 明文保存在本机 `%APPDATA%\StardewModManager\settings.json`，
  仅用于内嵌浏览器登录与 API 请求，不会上传到任何第三方
- 仓库中的 `*.key` 一律被 `.gitignore` 忽略；请勿提交自己的密钥文件

## License

[MIT](LICENSE)
