; Alex installer (Inno Setup 6).  Build:  ISCC.exe installer\alex.iss
; Per-user install (no admin rights): %LOCALAPPDATA%\Programs\Alex

#define AppName "Alex"
#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#define AppExe "alex.exe"

[Setup]
AppId={{6B2C7E4A-9F1D-4C8B-A3E5-1D7F0A2B9C41}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Alex
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.19041
OutputDir=..\dist
OutputBaseFilename=Alex-Setup-{#AppVersion}
SetupIconFile=..\assets\alex.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
WizardStyle=modern
WizardImageFile=wizard-large.bmp
WizardSmallImageFile=wizard-small.bmp
Compression=lzma2/ultra64
SolidCompression=yes
CloseApplications=no
VersionInfoVersion={#AppVersion}
VersionInfoProductName={#AppName}
VersionInfoDescription=Alex - hòn đảo nổi cho Windows

[Languages]
Name: "vi"; MessagesFile: "compiler:Default.isl,Vietnamese.isl"

[Tasks]
Name: "desktopicon"; Description: "Tạo biểu tượng trên màn hình Desktop"; GroupDescription: "Tuỳ chọn:"; Flags: unchecked
Name: "autostart"; Description: "Khởi động Alex cùng Windows"; GroupDescription: "Tuỳ chọn:"

[Files]
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; DestName: "Huong dan.md"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; same value the app's own "Khởi động cùng Windows" toggle uses
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Alex"; ValueData: """{app}\{#AppExe}"""; Tasks: autostart; Flags: uninsdeletevalue

[Run]
Filename: "{app}\{#AppExe}"; Description: "Mở Alex ngay"; Flags: nowait postinstall skipifsilent
; self-update runs the installer with /VERYSILENT /RELAUNCH=1: start the new version afterwards
Filename: "{app}\{#AppExe}"; Flags: nowait; Check: WantRelaunch

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/IM {#AppExe} /F"; Flags: runhidden; RunOnceId: "StopAlex"

[UninstallDelete]
Type: filesandordirs; Name: "{app}"

[Code]
function WantRelaunch(): Boolean;
begin
  Result := ExpandConstant('{param:RELAUNCH|0}') = '1';
end;

// Close a running Alex before files are replaced (upgrade / reinstall).
procedure StopAlex();
var
  Code: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/IM {#AppExe} /F', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Sleep(400);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  StopAlex();
  Result := '';
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
  begin
    // the app may also have written its own autostart entry
    RegDeleteValue(HKEY_CURRENT_USER, 'Software\Microsoft\Windows\CurrentVersion\Run', 'Alex');
    if DirExists(ExpandConstant('{userappdata}\Alex')) then
      // silent uninstall answers "No": data is never removed without a person saying so
      if SuppressibleMsgBox('Xoá luôn dữ liệu của Alex (ghi chú, bookmark, cài đặt) trong %APPDATA%\Alex?', mbConfirmation, MB_YESNO or MB_DEFBUTTON2, IDNO) = IDYES then
        DelTree(ExpandConstant('{userappdata}\Alex'), True, True, True);
  end;
end;
