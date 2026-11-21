; BatteryLife installer (Inno Setup 6). Per-user install: no admin prompt, no UAC.
; Build: ISCC /DAppVersion=0.2.0 /DSourceDir=<app build folder> /DOutputDir=<folder> installer\BatteryLife.iss
; The release workflow (.github/workflows/release.yml) runs this on every version tag.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #error Pass /DSourceDir=<folder with BatteryLife.exe>
#endif
; Windows file versions are numeric only: "0.3.0-beta" becomes "0.3.0" there.
#define NumericVersion Pos("-", AppVersion) > 0 ? Copy(AppVersion, 1, Pos("-", AppVersion) - 1) : AppVersion
#ifndef OutputDir
  #define OutputDir "."
#endif

[Setup]
; Never change AppId: Windows uses it to find the installed copy for upgrades and uninstall.
AppId={{F7151CAD-C450-4C58-9EBF-0BFEDDF18DDF}
AppName=BatteryLife
AppVersion={#AppVersion}
AppVerName=BatteryLife {#AppVersion}
AppPublisher=Josh Segatt
AppPublisherURL=https://github.com/joshsegatt/BatteryLife
AppSupportURL=https://github.com/joshsegatt/BatteryLife/issues
VersionInfoVersion={#NumericVersion}
; %LOCALAPPDATA%\Programs\BatteryLife for a per-user install.
DefaultDirName={autopf}\BatteryLife
DisableDirPage=yes
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.19041
OutputDir={#OutputDir}
OutputBaseFilename=BatteryLife-Setup-v{#AppVersion}
SetupIconFile=..\app\BatteryLife.App\Assets\app.ico
UninstallDisplayIcon={app}\BatteryLife.exe
UninstallDisplayName=BatteryLife
WizardStyle=modern
; Follow the Windows display language; ask only when it is neither English nor Portuguese.
ShowLanguageDialog=auto
; LZMA2 solid: about 180 MB of app files become a download of about 45 MB.
Compression=lzma2/ultra64
SolidCompression=yes
LZMAUseSeparateProcess=yes
; An upgrade closes the running tray app instead of asking the user to quit it.
CloseApplications=force
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "brazilianportuguese"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"

[CustomMessages]
english.StartWithWindows=Start BatteryLife with Windows
brazilianportuguese.StartWithWindows=Iniciar o BatteryLife com o Windows

[Tasks]
Name: "startup"; Description: "{cm:StartWithWindows}"

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\BatteryLife"; Filename: "{app}\BatteryLife.exe"

[Registry]
; Same value the app's own "Start with Windows" toggle writes (Startup.cs), so the menu shows it checked.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "BatteryLife"; ValueData: """{app}\BatteryLife.exe"""; Tasks: startup
; Uninstall removes the startup entry even when it was turned on from the app, and the app's settings.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "BatteryLife"; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\BatteryLife"; ValueType: none; Flags: uninsdeletekey

[Run]
Filename: "{app}\BatteryLife.exe"; Description: "{cm:LaunchProgram,BatteryLife}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; The tray app has no window to close politely; stop this copy (not a portable one elsewhere) so
; its files can be removed.
Filename: "powershell.exe"; Parameters: "-NoProfile -NonInteractive -Command ""Get-Process BatteryLife, batterylife-core -ErrorAction SilentlyContinue | Where-Object Path -like '{app}\*' | Stop-Process -Force"""; Flags: runhidden; RunOnceId: "StopBatteryLife"
