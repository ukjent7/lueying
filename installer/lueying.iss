; 掠影拼音输入法安装包。
;
; 安装包负责机器级动作:复制文件、注册 x64/x86 TSF、写卸载项。
; 当前用户状态(输入法列表、自启动、旧版快捷方式)交给 lueying-user.exe:
; 安装时用 runasoriginaluser 以原用户身份运行,卸载时随卸载器提权运行。
;
; 编译参数(xtask pack 与安装 E2E 共用):
;   /DPayloadDir=<发布目录>  /DOutDir=<输出目录>
;   /DVersion=<版本>  /DVersionSafe=<文件名安全版本>  /DVersionInfo=<x.y.z.w>

#ifndef AppName
  #define AppName "掠影"
#endif
#ifndef AppId
  #define AppId "{{5F0C8C3E-9B7A-4E2D-9A6B-2C1D8E4F7A10}"
#endif
#ifndef Version
  #define Version "0.0.0"
#endif
#ifndef VersionSafe
  #define VersionSafe Version
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
; 只有 x64 构建;ARM64 上 TSF 无法被原生宿主加载,直接拒绝安装。
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
Source: "{#PayloadDir}\lueying-settings.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\lueying-update.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\lueying-netproxy.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\lueying-user.exe"; DestDir: "{app}"; Flags: ignoreversion
; PrepareToInstall 在复制文件前就要停掉旧服务,所以再嵌一份临时副本。
Source: "{#PayloadDir}\lueying-user.exe"; DestDir: "{tmp}"; Flags: dontcopy noencryption
; 版本号进 DLL 文件名:升级时新文件不会被宿主加载的旧 DLL 锁住。
Source: "{#PayloadDir}\lueying_tsf.dll"; DestDir: "{app}"; DestName: "lueying_tsf-{#VersionSafe}.dll"; \
    Flags: ignoreversion regserver 64bit uninsrestartdelete
Source: "{#PayloadDir}\lueying_tsf32.dll"; DestDir: "{app}"; DestName: "lueying_tsf32-{#VersionSafe}.dll"; \
    Flags: ignoreversion regserver 32bit uninsrestartdelete skipifsourcedoesntexist
Source: "{#PayloadDir}\version.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\manifest.json"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#PayloadDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#PayloadDir}\licenses\*"; DestDir: "{app}\licenses"; \
    Flags: ignoreversion recursesubdirs createallsubdirs skipifsourcedoesntexist
Source: "{#PayloadDir}\main.tsv"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
; 安装冒烟只验证安装/注册/卸载,发布目录不带词典与模型;发版时 xtask
; 会先确认这些文件齐全,--allow-missing-data 只用于本地调试。
Source: "{#PayloadDir}\main.pyd"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#PayloadDir}\main.gram"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#PayloadDir}\rerank-4m.safetensors"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
; 25M 停顿重排模型与 PP-OCR 模型不随基础安装包分发;设置窗的「官方扩展」
; 按需下载,逐文件校验 SHA-256 后写入 %APPDATA%\lueying\models。
; 官方起步扩展(截图 OCR 等)随包分发,作为扩展中心的本地安装源;用户
; 在设置窗里点安装后才复制到 %APPDATA%\lueying\extensions。
Source: "{#PayloadDir}\extensions\*"; DestDir: "{app}\extensions"; \
    Flags: ignoreversion recursesubdirs createallsubdirs skipifsourcedoesntexist
; 官方扩展目录副本,供离线审计;运行时使用可执行文件里嵌入的同一份元数据。
Source: "{#PayloadDir}\Catalog\*"; DestDir: "{app}\Catalog"; \
    Flags: ignoreversion recursesubdirs createallsubdirs skipifsourcedoesntexist

[InstallDelete]
; 旧的自建安装器已被 lueying-user.exe 取代,不再随包分发。
Type: files; Name: "{app}\lueying-install.exe"

[Icons]
Name: "{group}\掠影设置"; Filename: "{app}\lueying-settings.exe"
Name: "{group}\卸载掠影"; Filename: "{uninstallexe}"

[Run]
; 输入法列表、自启动和旧版快捷方式都属于当前用户;Setup 是提权进程,
; 直接写 HKCU 会落到管理员账号上,所以交给以原用户身份运行的辅助程序。
Filename: "{app}\lueying-user.exe"; Parameters: "--install"; \
    StatusMsg: "正在完成当前用户设置..."; \
    Flags: runasoriginaluser runhidden waituntilterminated
; 旧版本 DLL 可能仍被宿主加载,删不掉的留给下次。Inno 不检查 [Run]
; 进程退出码,所以 prune 失败不会中断安装。
Filename: "{app}\lueying-user.exe"; Parameters: "--prune --keep {#VersionSafe}"; \
    Flags: runhidden waituntilterminated

[UninstallRun]
Filename: "{app}\lueying-user.exe"; Parameters: "--uninstall"; \
    Flags: runhidden waituntilterminated; RunOnceId: "LueyingUserCleanup"

[Code]
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
