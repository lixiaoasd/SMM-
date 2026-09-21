; 星露谷物语模组管理器 —— Inno Setup 安装脚本
; 编译：ISCC.exe setup.iss  →  Output\FireSVM-ModManager-Setup-<ver>.exe
#define MyAppName "星露谷物语模组管理器"
#define MyAppVersion "a0.01"
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
OutputBaseFilename=FireSVM-ModManager-Setup-a0.01
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
SetupIconFile=..\assets\app.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
ShowLanguageDialog=no

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

[Code]
function IsWebView2Installed: Boolean;
begin
  Result :=
    RegKeyExists(HKLM, 'SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}') or
    RegKeyExists(HKLM, 'SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}') or
    RegKeyExists(HKCU, 'Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}');
end;

function InitializeSetup(): Boolean;
begin
  Result := True;
  if not IsWebView2Installed then
  begin
    if MsgBox('未检测到 WebView2 运行时（Windows 11 通常已自带）。' #13#10
      + '缺少它时，管理器内置的网页浏览功能将无法使用。' #13#10 #13#10
      + '点「是」继续安装（稍后可自行安装 WebView2 Evergreen Runtime）；' #13#10
      + '点「否」退出安装。',
      mbConfirmation, MB_YESNO) = IDNO then
      Result := False;
  end;
end;
