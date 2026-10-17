# Installs prev on the clean Windows it runs on, a Windows container
# without the Visual C++ Redistributable or anything else, and checks that
# it starts: the MSI, silently, as winget installs it, then the zip. Run by
# .github/workflows/install-test.yml:
#
#   install-test.ps1 -Packages C:\packages [-SystemDlls C:\packages\system-dlls.txt]
#
# SystemDlls lists the Windows DLLs prev loads (check-dlls.py
# --system-list), to name the one missing should prev not start.
param(
    [Parameter(Mandatory)] [string] $Packages,
    [string] $SystemDlls
)
$ErrorActionPreference = 'Stop'

function Test-Starts([string] $exe) {
    $out = Join-Path $env:TEMP 'version.txt'
    $run = Start-Process $exe -ArgumentList '--version' -Wait -PassThru -NoNewWindow -RedirectStandardOutput $out
    $version = (Get-Content -Raw $out -ErrorAction SilentlyContinue)
    if ($run.ExitCode -ne 0 -or $version -notmatch '^prev \d') {
        $code = '0x{0:X8}' -f $run.ExitCode
        $why = if ($code -eq '0xC0000135') { ': a DLL it loads is missing' } else { '' }
        $dir = Split-Path $exe
        foreach ($dll in @(Get-ChildItem $dir -Filter *.dll | ForEach-Object Name)) { Write-Host "  beside it: $dll" }
        if ($SystemDlls) {
            foreach ($dll in Get-Content $SystemDlls) {
                if (-not (Test-Path (Join-Path $env:windir "System32\$dll"))) { Write-Host "  not in this Windows: $dll" }
            }
        }
        throw "$exe --version exited with $code$why"
    }
    Write-Host "$exe starts: $($version.Trim())"
}

# The test means something only if this Windows has no runtime to lend.
foreach ($dll in 'msvcp140.dll', 'vcruntime140.dll') {
    if (Test-Path (Join-Path $env:windir "System32\$dll")) {
        throw "$dll is already in System32: this Windows is not clean"
    }
}
Write-Host "Windows $([Environment]::OSVersion.Version), without the Visual C++ runtime"

$msi = Get-ChildItem $Packages -Filter 'prev-*-x64.msi' | Select-Object -First 1
if (-not $msi) { throw "No x64 MSI in $Packages" }
$log = Join-Path $env:TEMP 'install.log'
$install = Start-Process msiexec.exe -ArgumentList '/i', "`"$($msi.FullName)`"", '/qn', '/norestart', '/l*v', "`"$log`"" -Wait -PassThru
if ($install.ExitCode -ne 0) {
    Get-Content $log -Tail 40
    throw "Installing $($msi.Name) exited with $($install.ExitCode)"
}
$exe = Join-Path $env:LOCALAPPDATA 'Programs\prev\prev.exe'
if (-not (Test-Path $exe)) { throw "The MSI did not install $exe" }
Test-Starts $exe

$uninstall = Start-Process msiexec.exe -ArgumentList '/x', "`"$($msi.FullName)`"", '/qn', '/norestart' -Wait -PassThru
if ($uninstall.ExitCode -ne 0) { throw "Uninstalling exited with $($uninstall.ExitCode)" }
if (Test-Path (Split-Path $exe)) { throw "Uninstalling left $(Split-Path $exe) behind" }
Write-Host "$($msi.Name) installs, starts and uninstalls"

$zip = Get-ChildItem $Packages -Filter 'prev-*-x64-windows.zip' | Select-Object -First 1
if (-not $zip) { throw "No x64 zip in $Packages" }
$portable = Join-Path $env:TEMP 'prev-portable'
Expand-Archive $zip.FullName -DestinationPath $portable -Force
Test-Starts (Join-Path $portable 'prev.exe')
Write-Host "$($zip.Name) starts"
