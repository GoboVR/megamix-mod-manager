; Mega Mix+ Mod Manager installer (Inno Setup 6.3 or newer).
;
;   iscc /DArch=x64   /DVersion=0.1.0 /DBinDir=path\to\x64\exes   packaging\windows\megamix.iss
;   iscc /DArch=arm64 /DVersion=0.1.0 /DBinDir=path\to\arm64\exes packaging\windows\megamix.iss
;
; Installs for the current user (no admin needed): adds the app to the Start
; menu, puts `megamix` (the command line tool) on PATH, and can open
; DivaModArchive / GameBanana one-click install links.

#ifndef Arch
  #define Arch "x64"
#endif
#ifndef Version
  #define Version "0.1.0"
#endif
#ifndef BinDir
  #define BinDir "..\..\target\release"
#endif

#define AppName "Mega Mix+ Mod Manager"
#define Scheme "divamodmanager"

[Setup]
AppId={{6B8E3E52-3C51-4E39-9B57-6D6C2F1A9A41}
AppName={#AppName}
AppVersion={#Version}
AppVerName={#AppName} {#Version}
AppPublisher=GoboVR
AppPublisherURL=https://github.com/GoboVR/megamix-mod-manager
AppSupportURL=https://github.com/GoboVR/megamix-mod-manager/issues
AppUpdatesURL=https://github.com/GoboVR/megamix-mod-manager/releases
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ChangesEnvironment=yes
ChangesAssociations=yes
CloseApplications=yes
OutputDir=..\..\dist
OutputBaseFilename=MegaMixModManager-{#Version}-Setup-{#Arch}
SetupIconFile=..\..\assets\icon.ico
UninstallDisplayIcon={app}\megamix-gui.exe
UninstallDisplayName={#AppName}
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
LicenseFile=..\..\LICENSE-MIT
#if Arch == "arm64"
ArchitecturesAllowed=arm64
ArchitecturesInstallIn64BitMode=arm64
#else
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
#endif

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "path"; Description: "Let me run ""megamix"" (the command line tool) from any terminal"; GroupDescription: "Command line:"
Name: "links"; Description: "Open one-click install links from DivaModArchive and GameBanana with this app"; GroupDescription: "Websites:"
Name: "steam"; Description: "Add it to Steam as a non-Steam game (for Big Picture mode; Steam restarts if it's open)"; GroupDescription: "Steam:"; Flags: unchecked
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#BinDir}\megamix-gui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\megamix.exe"; DestDir: "{app}\bin"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\assets\steam\*.png"; DestDir: "{app}\steam"; Flags: ignoreversion
Source: "..\..\assets\icon-256.png"; DestDir: "{app}"; DestName: "icon.png"; Flags: ignoreversion
Source: "..\..\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\megamix-gui.exe"; Comment: "Install and manage mods for Project DIVA Mega Mix+"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\megamix-gui.exe"; Tasks: desktopicon

[Registry]
; One-click links (same keys the app's own "Register" button writes).
Root: HKA; Subkey: "Software\Classes\{#Scheme}"; ValueType: string; ValueName: ""; ValueData: "URL:Mega Mix+ Mod Manager one-click install"; Tasks: links
Root: HKA; Subkey: "Software\Classes\{#Scheme}"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""; Tasks: links
Root: HKA; Subkey: "Software\Classes\{#Scheme}\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\megamix-gui.exe"",0"; Tasks: links
Root: HKA; Subkey: "Software\Classes\{#Scheme}\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\megamix-gui.exe"" ""%1"""; Tasks: links

[Run]
Filename: "{app}\bin\megamix.exe"; Parameters: "steam-shortcut add --close-steam --art ""{app}\steam"" --icon ""{app}\icon.png"""; StatusMsg: "Adding it to Steam…"; Flags: runhidden waituntilterminated; Tasks: steam
Filename: "{app}\megamix-gui.exe"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
; After an update started from inside the app (/RELAUNCH=1): open it again.
Filename: "{app}\megamix-gui.exe"; Flags: nowait; Check: ShouldRelaunch

[Code]
function ShouldRelaunch: Boolean;
begin
  Result := ExpandConstant('{param:RELAUNCH|0}') = '1';
end;

const
  UserEnvKey = 'Environment';
  SystemEnvKey = 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment';

function EnvRoot: Integer;
begin
  if IsAdminInstallMode then Result := HKEY_LOCAL_MACHINE else Result := HKEY_CURRENT_USER;
end;

function EnvKey: String;
begin
  if IsAdminInstallMode then Result := SystemEnvKey else Result := UserEnvKey;
end;

{ Is Dir one of the ;-separated entries of Path (ignoring case and a trailing \)? }
function PathHas(Path, Dir: String): Boolean;
begin
  Result := Pos(';' + Lowercase(RemoveBackslashUnlessRoot(Dir)) + ';',
                ';' + Lowercase(Path) + ';') > 0;
  if not Result then
    Result := Pos(';' + Lowercase(AddBackslash(Dir)) + ';', ';' + Lowercase(Path) + ';') > 0;
end;

procedure AddToPath(Dir: String);
var
  Path: String;
begin
  if not RegQueryStringValue(EnvRoot, EnvKey, 'Path', Path) then Path := '';
  if PathHas(Path, Dir) then exit;
  if (Path <> '') and (Copy(Path, Length(Path), 1) <> ';') then Path := Path + ';';
  Path := Path + Dir;
  { REG_EXPAND_SZ keeps %VARIABLES% in other entries working. }
  RegWriteExpandStringValue(EnvRoot, EnvKey, 'Path', Path);
end;

procedure RemoveFromPath(Dir: String);
var
  Path, Entry, Rest, NewPath: String;
  P: Integer;
begin
  if not RegQueryStringValue(EnvRoot, EnvKey, 'Path', Path) then exit;
  NewPath := '';
  Rest := Path;
  while Rest <> '' do
  begin
    P := Pos(';', Rest);
    if P = 0 then
    begin
      Entry := Rest;
      Rest := '';
    end else begin
      Entry := Copy(Rest, 1, P - 1);
      Rest := Copy(Rest, P + 1, Length(Rest));
    end;
    if (Entry <> '') and
       (CompareText(RemoveBackslashUnlessRoot(Entry), RemoveBackslashUnlessRoot(Dir)) <> 0) then
    begin
      if NewPath <> '' then NewPath := NewPath + ';';
      NewPath := NewPath + Entry;
    end;
  end;
  if NewPath <> Path then
    RegWriteExpandStringValue(EnvRoot, EnvKey, 'Path', NewPath);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    if WizardIsTaskSelected('path') then
      AddToPath(ExpandConstant('{app}\bin'))
    else
      RemoveFromPath(ExpandConstant('{app}\bin'));
  end;
end;

{ Only remove the link handler if it still points at this app (another mod
  manager may have taken it over since). }
procedure RemoveLinkHandlerIfOurs;
var
  Cmd, Key: String;
  Root: Integer;
begin
  if IsAdminInstallMode then Root := HKEY_LOCAL_MACHINE else Root := HKEY_CURRENT_USER;
  Key := 'Software\Classes\{#Scheme}';
  if RegQueryStringValue(Root, Key + '\shell\open\command', '', Cmd) then
    if Pos(Lowercase(ExpandConstant('{app}')), Lowercase(Cmd)) > 0 then
      RegDeleteKeyIncludingSubkeys(Root, Key);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  ResultCode: Integer;
begin
  if CurUninstallStep = usUninstall then
  begin
    RemoveFromPath(ExpandConstant('{app}\bin'));
    RemoveLinkHandlerIfOurs;
    { Take the Steam shortcut out too, if Steam is closed (never force it). }
    Exec(ExpandConstant('{app}\bin\megamix.exe'), 'steam-shortcut remove', '', SW_HIDE,
         ewWaitUntilTerminated, ResultCode);
  end;
end;
