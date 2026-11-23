; SnapForge installer (Inno Setup 6). Per-user install, no admin rights required.
; Build: scripts\package.ps1 (or: ISCC.exe /DAppVersion=0.1.0 installer\SnapForge.iss)

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
; Cargo's output folder (package.ps1 passes CARGO_TARGET_DIR when it is set).
#ifndef TargetDir
  #define TargetDir "..\target"
#endif

[Setup]
AppId={{6E7B2F3A-9C41-4B8E-A5D2-5F0C3E9A1B77}
AppName=SnapForge
AppVersion={#AppVersion}
AppVerName=SnapForge {#AppVersion}
AppPublisher=Zheer Khalid
AppPublisherURL=https://zheer.dev
AppSupportURL=https://github.com/zheerkhalid/snapforge
AppUpdatesURL=https://github.com/zheerkhalid/snapforge/releases
DefaultDirName={autopf}\SnapForge
DefaultGroupName=SnapForge
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible arm64
ArchitecturesInstallIn64BitMode=x64compatible arm64
OutputDir=..\dist
OutputBaseFilename=SnapForge-Setup
SetupIconFile={#TargetDir}\snapforge.ico
UninstallDisplayIcon={app}\SnapForge.exe
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no
LicenseFile=..\LICENSE
VersionInfoVersion={#AppVersion}
VersionInfoCompany=Zheer Khalid (zheer.dev)
VersionInfoDescription=SnapForge Setup

[Tasks]
Name: "startup"; Description: "Start SnapForge with Windows"; GroupDescription: "Options:"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#TargetDir}\release\SnapForge.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\docs\USAGE.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\SnapForge"; Filename: "{app}\SnapForge.exe"
Name: "{group}\Uninstall SnapForge"; Filename: "{uninstallexe}"
Name: "{autodesktop}\SnapForge"; Filename: "{app}\SnapForge.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "SnapForge"; ValueData: """{app}\SnapForge.exe"" --tray"; Flags: uninsdeletevalue; Tasks: startup

[Run]
Filename: "{app}\SnapForge.exe"; Description: "{cm:LaunchProgram,SnapForge}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{cmd}"; Parameters: "/C taskkill /IM SnapForge.exe /F"; Flags: runhidden; RunOnceId: "KillSnapForge"
Filename: "{app}\SnapForge.exe"; Parameters: "--uninstall-cleanup"; Flags: runhidden waituntilterminated; RunOnceId: "Cleanup"
