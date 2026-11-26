; AgnoPeriph Windows x64 Installer (Inno Setup 6)
; Compiles target\release\agnoperiph.exe and agnoperiph-cli.exe into dist\AgnoPeriph-Setup-x64.exe.

#ifndef AppVersion
  #define AppVersion "0.1.0.0"
#endif

#ifndef RepoRoot
  #define RepoRoot "..\.."
#endif

[Setup]
AppId={{8F3B4D12-5C61-4D87-A9E4-2E9D1C7B6A30}
AppName=AgnoPeriph
AppVersion={#AppVersion}
AppVerName=AgnoPeriph {#AppVersion}
AppPublisher=AgnoPeriph Contributors
AppPublisherURL=https://github.com/D9theCoder/AgnoPeriph
AppSupportURL=https://github.com/D9theCoder/AgnoPeriph/issues
AppUpdatesURL=https://github.com/D9theCoder/AgnoPeriph/releases
VersionInfoVersion={#AppVersion}
DefaultDirName={autopf}\AgnoPeriph
DefaultGroupName=AgnoPeriph
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
SetupIconFile={#RepoRoot}\crates\ui\assets\logo.ico
UninstallDisplayIcon={app}\agnoperiph.exe
UninstallDisplayName=AgnoPeriph
OutputDir={#RepoRoot}\dist
OutputBaseFilename=AgnoPeriph-Setup-x64
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=force
CloseApplicationsFilter=agnoperiph.exe
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#RepoRoot}\target\release\agnoperiph.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\target\release\agnoperiph-cli.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\AgnoPeriph"; Filename: "{app}\agnoperiph.exe"; AppUserModelID: "AgnoPeriph.App"
Name: "{autodesktop}\AgnoPeriph"; Filename: "{app}\agnoperiph.exe"; Tasks: desktopicon; AppUserModelID: "AgnoPeriph.App"

[Run]
Filename: "{app}\agnoperiph.exe"; Description: "{cm:LaunchProgram,AgnoPeriph}"; Flags: nowait postinstall skipifsilent
Filename: "{app}\agnoperiph.exe"; Flags: nowait skipifnotsilent; Check: ShouldRelaunchAfterSilentUpdate

[Code]
function ShouldRelaunchAfterSilentUpdate: Boolean;
begin
  Result := WizardSilent and (ExpandConstant('{param:relaunch|0}') = '1');
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
  begin
    RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', 'AgnoPeriph');
  end;
end;
