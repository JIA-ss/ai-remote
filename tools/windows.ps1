# Native Windows development and packaging, including the command and the background host the app
# installs from inside its own folder (Settings > This device > Enable remote control).
#
#   pwsh tools/windows.ps1 [dev|build] [-DryRun] [Tauri arguments...]
#
# build: cargo builds ai-remote and ai-remote-hostw, copies them into Tauri's externalBin folder with
# the target suffix, and bundles the app with tauri.windows.bundle.conf.json, so the installer puts
# ai-remote.exe and ai-remote-hostw.exe next to the app. -DryRun prints the plan and runs nothing.
# Arguments are read by hand (no param block): Tauri flags such as --debug must reach Tauri as they are.
$Mode = 'dev'
$modeSet = $false
$DryRun = $false
$TauriArgs = @()
foreach ($a in $args) {
    if ($a -eq '-DryRun' -or $a -eq '--dry-run') { $DryRun = $true }
    elseif (-not $modeSet -and ($a -eq 'dev' -or $a -eq 'build')) { $Mode = $a; $modeSet = $true }
    else { $TauriArgs += $a }
}
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$debugBuild = ($Mode -eq 'dev') -or ($TauriArgs -contains '--debug')
$buildProfile = if ($debugBuild) { 'debug' } else { 'release' }
$cargoArgs = @('build', '-p', 'ai-remote') + $(if ($debugBuild) { @() } else { @('--release') })
$tauri = if ($Mode -eq 'build') {
    @('run', 'tauri', '--', 'build', '--config', 'src-tauri/tauri.windows.bundle.conf.json') + $TauriArgs
} else {
    @('run', 'tauri', '--', 'dev') + $TauriArgs
}

if ($DryRun) {
    Write-Output "Windows $Mode plan ($buildProfile); the native target is resolved with rustc -vV when executed."
    Write-Output ("cargo " + ($cargoArgs -join ' '))
    Write-Output "Copy ai-remote.exe and ai-remote-hostw.exe into apps/desktop/src-tauri/binaries with the target suffix."
    Write-Output ("npm " + ($tauri -join ' '))
    exit 0
}

if (-not $IsWindows -and $PSVersionTable.PSEdition -eq 'Core') {
    Write-Error 'This helper needs Windows.'
}
& cargo @cargoArgs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$target = (& rustc -vV | Select-String '^host: ').ToString().Substring(6).Trim()
$bin = Join-Path $root 'apps/desktop/src-tauri/binaries'
New-Item -ItemType Directory -Force $bin | Out-Null
foreach ($name in 'ai-remote', 'ai-remote-hostw') {
    Copy-Item (Join-Path $root "target/$buildProfile/$name.exe") (Join-Path $bin "$name-$target.exe") -Force
}
$env:AI_REMOTE_CLI = Join-Path $root "target/$buildProfile/ai-remote.exe"
Set-Location (Join-Path $root 'apps/desktop')
if (-not (Test-Path 'node_modules')) { & npm ci; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE } }
& npm @tauri
exit $LASTEXITCODE
