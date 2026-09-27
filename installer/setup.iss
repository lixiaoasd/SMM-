; 星露谷物语模组管理器 —— Inno Setup 安装脚本
; 编译：ISCC.exe setup.iss  →  Output\FireSVM-ModManager-Setup-<ver>.exe
#define MyAppName "星露谷物语模组管理器"
#define MyAppVersion "b0.7"
; 纯数字版本号，用于安装包的版本信息资源（杀软信誉 / SmartScreen 会读这些字段）
#define MyAppVersionNumeric "0.7.0"
#define MyAppPublisher "FireSVM"
#define MyAppExeName "stardew-mod-manager.exe"

[Setup]
AppId={{258BF26E-D6F1-4CD6-A83C-855CAB0D0420}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
DefaultDirName={localappdata}\Programs\FireSVM-ModManager
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=Output
OutputBaseFilename=FireSVM-ModManager-Setup-b0.7
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
SetupIconFile=..\assets\app.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
ShowLanguageDialog=no
; 版本信息资源：与主程序保持一致（发布者/产品/版本），杀软信誉与 SmartScreen 会读
VersionInfoVersion={#MyAppVersionNumeric}.0
VersionInfoProductVersion={#MyAppVersionNumeric}
VersionInfoCompany={#MyAppPublisher}
VersionInfoProductName=Stardew Valley Mod Manager
VersionInfoDescription={#MyAppName} 安装程序
VersionInfoCopyright=MIT License

[Languages]
Name: "chinesesimplified"; MessagesFile: "Languages\ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: checkedonce

[Files]
Source: "..\target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{group}\卸载 {#MyAppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#MyAppName}}"; Flags: nowait postinstall skipifsilent
