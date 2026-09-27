# 星露谷物语模组管理器（FireSVM Mod Manager）

Windows 桌面端的《星露谷物语》模组管理器：浏览与安装模组、管理已装模组、监控浏览器下载并自动安装、一键装 SMAPI 并把 Steam 启动项指向 SMAPI。

> 一键开服和联机大厅暂不可用

## 功能

- **模组库**：浏览镜像站收录的模组，一键直装；自动补齐缺失前置、检测清单声明的冲突
- **我的模组**：本地已装模组列表、启用/停用、删除、打开目录
- **Mods 文件夹监控**：浏览器下载回来的模组压缩包自动识别并安装（含汉化包合并）
- **SMAPI**：一键下载安装；安装后自动把 Steam 启动项指向 `StardewModdingAPI.exe`
- **联机 / 开服辅助**：当前不可用

## 下载与安装

到 [Releases](https://github.com/lixiaoasd/SMM-/releases) 下载 `FireSVM-ModManager-Setup-bX.Y.exe` 双击安装。安装到 `%LOCALAPPDATA%\Programs\FireSVM-ModManager`，不需要管理员权限。

## 校验下载文件（SHA256SUMS）

每次发布都会附带 `SHA256SUMS.txt`。下载后在本目录执行：

```powershell
Get-FileHash .\FireSVM-ModManager-Setup-b0.7.exe -Algorithm SHA256
Select-String -Path .\SHA256SUMS.txt -Pattern "FireSVM-ModManager-Setup-b0.7.exe"
```

两处哈希一致即说明文件完整、未被替换。

（发布者侧生成同一份文件的命令）

```powershell
Get-FileHash .\FireSVM-ModManager-Setup-b0.7.exe -Algorithm SHA256 |
  ForEach-Object { "$($_.Hash.ToLower())  $($_.Path | Split-Path -Leaf)" } |
  Set-Content SHA256SUMS.txt -Encoding ascii
```

## 代码签名

当前发布的安装包与主程序**尚未做数字签名**，安装时 Windows 可能提示"未知发布者"。我们计划申请 [SignPath Foundation](https://signpath.org/) 面向开源项目的免费代码签名（证书由 SignPath Foundation 签发）；启用后本节会更新为它要求的措辞：

> Free code signing provided by SignPath.io, certificate by SignPath Foundation.

在签名启用之前，请以 Release 页附带的 `SHA256SUMS.txt` 校验下载文件。

## 隐私说明

- **不收集、不上传任何个人信息**，不含统计或遥测代码。
- **联网只发生在**：请求模组镜像站（默认 `http://116.62.231.162`）拉取清单与下载模组、请求 GitHub / Nexus Mods API 查询版本信息、下载 SMAPI。请求内容仅为模组与版本查询，不含你的本地信息。
- **本地读取**：游戏目录与 `Mods` 目录、SMAPI 日志、Steam 安装路径，以及当前 Steam 账号的配置文件（仅用于"把启动项指向 SMAPI"这一功能）。
- **本地写入**：游戏目录（安装/更新模组）、`%APPDATA%\StardewModManager`（设置、缓存、日志）。设置页的「Steam 启动项」按钮会在你**主动点击**后写入 Steam 启动项，写入前自动备份到 `%APPDATA%\StardewModManager\backup`，可在设置页一键恢复；Steam 正在运行时一律拒绝写入。
- **N 网 API Key**（可选，留空即不使用）只保存在本机 `%APPDATA%\StardewModManager\settings.json`，只用于代替你向 Nexus Mods API 发起请求。

## 从源码构建

```bash
bash build.sh            # 需要 Rust GNU 工具链 + mingw-w64 binutils（见脚本头部说明）
bash build.sh locked     # 发布锁定版：镜像地址固定为默认服务器
```

安装包用 Inno Setup 编译：`ISCC.exe installer/setup.iss`。

## 许可

[MIT](LICENSE)