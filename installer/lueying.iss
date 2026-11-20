; 掠影拼音输入法安装包。
;
; 安装包负责机器级动作:复制文件、注册 x64 TSF、写卸载项。
; 当前用户状态(输入法列表、自启动、旧版快捷方式)交给 lueying-user.exe:
; 安装时用 runasoriginaluser 以原用户身份运行,卸载时随卸载器提权运行。
;
; 编译参数(xtask pack 与安装 E2E 共用):
;   /DPayloadDir=<发布目录>  /DOutDir=<输出目录>
;   /DVersion=<版本>  /DVersionInfo=<x.y.z.w>

#ifndef AppName
  #define AppName "掠影"
#endif
#ifndef AppId
  #define AppId "{{5F0C8C3E-9B7A-4E2D-9A6B-2C1D8E4F7A10}"
#endif
#ifndef Version
  #define Version "0.0.0"
#endif
#ifndef VersionInfo
  #define VersionInfo "0.0.0.0"
#endif
#ifndef PayloadDir
  #define PayloadDir "."
#endif
#ifndef OutDir
  #define OutDir "Output"
#endif

[Setup]
AppId={#AppId}
AppName={#AppName}
AppVersion={#Version}
AppPublisher={#AppName}
AppPublisherURL=https://github.com/ukjent7/lueying
AppSupportURL=https://github.com/ukjent7/lueying
AppUpdatesURL=https://github.com/ukjent7/lueying/releases
DefaultDirName={autopf}\lueying
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
OutputDir={#OutDir}
OutputBaseFilename=lueying-{#Version}-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern dynamic
PrivilegesRequired=admin
; 目标系统 Windows 11 24H2(build 26100)起;只有 x64 构建,ARM64 直接拒绝。
MinVersion=10.0.26100
ArchitecturesAllowed=x64compatible and not arm64
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayName={#AppName}
UninstallDisplayIcon={app}\lueying-settings.exe
VersionInfoVersion={#VersionInfo}
VersionInfoDescription={#AppName} 安装程序
VersionInfoProductName={#AppName}
SetupLogging=yes

[Languages]
Name: "chinesesimplified"; MessagesFile: "ChineseSimplified.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
Source: "{#PayloadDir}\lueying-server.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\lueying-ext-host.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\lueying-settings.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\lueying-user.exe"; DestDir: "{app}"; Flags: ignoreversion
; PrepareToInstall 在复制文件前就要停掉旧服务,所以再嵌一份临时副本。
Source: "{#PayloadDir}\lueying-user.exe"; DestDir: "{tmp}"; Flags: dontcopy noencryption
; TSF 组件文件名固定:升级覆盖同一文件,Restart Manager 才能把正在用
; 旧组件的应用列出来征询关闭并在装完重启;仍被宿主加载时由
; PrepareTsfDll 改名让路,注册表指向的路径因此长期稳定。
Source: "{#PayloadDir}\lueying_tsf.dll"; DestDir: "{app}"; DestName: "lueying_tsf.dll"; \
    Flags: ignoreversion regserver 64bit uninsrestartdelete; \
    BeforeInstall: PrepareTsfDll
Source: "{#PayloadDir}\version.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\manifest.json"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#PayloadDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#PayloadDir}\licenses\*"; DestDir: "{app}\licenses"; \
    Flags: ignoreversion recursesubdirs createallsubdirs skipifsourcedoesntexist
; 安装冒烟只验证安装/注册/卸载;发版时 xtask 会先确认词典与搭配表齐全,
; --allow-missing-data 只用于本地调试。
Source: "{#PayloadDir}\main.pyd"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#PayloadDir}\collocation.bin"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
; 两档神经重排模型与 PP-OCR 模型都不随包:体积大且很少变动,由设置窗按需
; 下载到用户数据目录(%LOCALAPPDATA%\lueying\models),升级与卸载都不动它。
; 模型缺失时引擎只保留词频候选,装完离线即可打字。

[InstallDelete]
; 旧的自建安装器已被 lueying-user.exe 取代,不再随包分发。
Type: files; Name: "{app}\lueying-install.exe"
; 历次升级改名让路的旧 TSF 组件;仍被宿主加载时删不掉,留给下次。
Type: files; Name: "{app}\lueying_tsf*.dll.previous"

[Icons]
Name: "{group}\掠影设置"; Filename: "{app}\lueying-settings.exe"
Name: "{group}\卸载掠影"; Filename: "{uninstallexe}"

[Run]
; 带 uiAccess 清单的 server 本体没有受信任根下的签名时,进程创建会被系统
; 直接拒绝,所以签名必须在任何东西尝试拉起服务之前做完。Inno 不检查
; [Run] 进程退出码,失败的痕迹在 LUEYING_INSTALL_TRACE。
Filename: "{app}\lueying-user.exe"; Parameters: "--sign-server"; \
    StatusMsg: "正在建立签名信任..."; \
    Flags: runhidden waituntilterminated
; 输入法列表、自启动和旧版快捷方式都属于当前用户;Setup 是提权进程,
; 直接写 HKCU 会落到管理员账号上,所以交给以原用户身份运行的辅助程序。
Filename: "{app}\lueying-user.exe"; Parameters: "--install"; \
    StatusMsg: "正在完成当前用户设置..."; \
    Flags: runasoriginaluser runhidden waituntilterminated
; 旧版本 DLL 可能仍被宿主加载,删不掉的留给下次。Inno 不检查 [Run]
; 进程退出码,所以 prune 失败不会中断安装。
Filename: "{app}\lueying-user.exe"; Parameters: "--prune"; \
    Flags: runhidden waituntilterminated

[UninstallRun]
Filename: "{app}\lueying-user.exe"; Parameters: "--uninstall"; \
    Flags: runhidden waituntilterminated; RunOnceId: "LueyingUserCleanup"

[Code]
// 宿主进程仍加载着旧 TSF 组件时,覆盖同名文件会失败。改名让路:
// 已加载的模块照常运行,新进程从原名拿到新文件。让路名带时间戳:
// 上一次让路的文件可能仍被长期宿主(explorer、SearchHost 等)加载而
// 删不掉,固定名会让本次改名撞名失败,进而整个升级中止回滚。
// 旧让路文件由 [InstallDelete] 与 --prune 在能删时清掉。
procedure PrepareTsfDll;
var
  Target, Previous: String;
begin
  Target := ExpandConstant('{app}\lueying_tsf.dll');
  if not FileExists(Target) then
    exit;
  Previous := ExpandConstant('{app}\lueying_tsf-') +
    GetDateTimeString('yyyymmddhhnnsszzz', '-', '-') + '.dll.previous';
  RenameFile(Target, Previous);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Helper: String;
  ResultCode: Integer;
begin
  NeedsRestart := False;
  Result := '';
  { 复制文件前先停掉常驻服务,否则 lueying-server.exe 会被占用。
    Setup 已提权时 ExecAsOriginalUser 可用;罕见情况下回退到提权执行,
    辅助程序按进程名停止服务,不依赖管道归属。 }
  ExtractTemporaryFile('lueying-user.exe');
  Helper := ExpandConstant('{tmp}\lueying-user.exe');
  if not ExecAsOriginalUser(Helper, '--stop-server', '', SW_HIDE,
      ewWaitUntilTerminated, ResultCode) then
    Exec(Helper, '--stop-server', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
end;
