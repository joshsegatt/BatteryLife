# Smoke test: the app must start and stay up. Catches startup crashes (XAML or WinRT errors) that
# compile fine. Usage: pwsh app/tools/smoke.ps1 <path to BatteryLife.exe>
param([Parameter(Mandatory)] [string]$Exe)
$p = Start-Process $Exe -ArgumentList '--demo', '--open' -PassThru
Start-Sleep -Seconds 8
if ($p.HasExited) {
    Write-Error "BatteryLife exited during startup with code $($p.ExitCode)"
    exit 1
}
Stop-Process $p
"smoke ok: BatteryLife stayed up for 8 s"
