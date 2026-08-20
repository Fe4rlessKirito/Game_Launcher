#define MyAppName "Vaultnode"
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#define BuildRoot "..\artifacts\vaultnode-win-x64"

[Setup]
AppId={{A8E0D0D6-08B1-4A37-9D08-5A9F8A1E5A21}
AppName={#MyAppName}
AppVersion={#AppVersion}
AppPublisher=Vaultnode
AppPublisherURL=https://vaultnode.pp.ua
AppSupportURL=https://github.com/Fe4rlessKirito/Game_Launcher
AppUpdatesURL=https://github.com/Fe4rlessKirito/Game_Launcher/releases
DefaultDirName={localappdata}\Programs\Vaultnode
DefaultGroupName=Vaultnode
DisableProgramGroupPage=yes
OutputDir=..\artifacts
OutputBaseFilename=Vaultnode-Setup
SetupIconFile=..\launcher\src\Launcher.App\Assets\vaultnode.ico
UninstallDisplayIcon={app}\Launcher.App.exe
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=lowest
CloseApplications=yes
RestartApplications=yes

[Files]
Source: "{#BuildRoot}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\Vaultnode"; Filename: "{app}\Launcher.App.exe"; WorkingDir: "{app}"

[Run]
Filename: "{app}\Launcher.App.exe"; Description: "Launch Vaultnode"; Flags: nowait postinstall skipifsilent
