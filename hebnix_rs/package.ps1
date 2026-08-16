<#
build release + assemble a clean dist/ with only what ships. nothing from
target/ (no deps/, build/, .ilk, .d...).

dist/ =
  hebnix-app.exe
  steam_api64.dll         eos steam auth
  rlapi-bridge.exe        psynet bridge (from rlapi_bridge/dist/)
  curl-impersonate/       tracker.gg tls-fingerprint bypass (exe+cacert)

pdb (target/release/hebnix_app.pdb, big) is not shipped by default. keep it
archived per release so you can symbolicate a user's crash.txt later. -WithPdb
bundles it in dist/ instead (readable crashes on their machine, way bigger dl).
#>
param([switch]$WithPdb)

$ErrorActionPreference = 'Stop'
$root       = $PSScriptRoot                       # hebnix_rs
$repoRoot   = Split-Path $root -Parent            # RShebnix
$releaseDir = Join-Path $root 'target\release'
$distDir    = Join-Path $root 'dist'
$bridgeExe  = Join-Path $repoRoot 'rlapi_bridge\dist\rlapi-bridge.exe'
$steamDll   = Join-Path $root 'vendor\steam_api64.dll'
$curlDir    = Join-Path $root 'vendor\curl-impersonate'

Write-Host 'building release...' -ForegroundColor Cyan
Push-Location $root
try {
    Get-Process hebnix-app -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    # cargo writes progress to stderr, which under ErrorActionPreference=Stop
    # would abort the script even on success. relax it for the build, then
    # check the real exit code.
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    cargo build --release
    $code = $LASTEXITCODE
    $ErrorActionPreference = $prev
    if ($code -ne 0) { throw "cargo build failed ($code)" }
} finally {
    Pop-Location
}

# everything we bundle must exist
if (-not (Test-Path $bridgeExe)) {
    throw "rlapi-bridge.exe missing at $bridgeExe`n  -> build it: rlapi_bridge\build.bat"
}
if (-not (Test-Path $steamDll)) {
    throw "steam_api64.dll missing at $steamDll (should be committed under vendor/)"
}
if (-not (Test-Path $curlDir)) {
    throw "curl-impersonate/ missing at $curlDir (should be committed under vendor/)"
}

Write-Host 'assembling dist/...' -ForegroundColor Cyan
if (Test-Path $distDir) { Remove-Item $distDir -Recurse -Force }
New-Item -ItemType Directory -Path $distDir | Out-Null

$files = @(
    (Join-Path $releaseDir 'hebnix-app.exe'),
    $steamDll,
    $bridgeExe
)
# rust names the exe hebnix-app.exe but the pdb hebnix_app.pdb (underscore),
# and that underscore name is what's baked into the exe, so ship it as-is.
if ($WithPdb) { $files += (Join-Path $releaseDir 'hebnix_app.pdb') }

foreach ($f in $files) {
    if (-not (Test-Path $f)) { throw "build output missing: $f" }
    Copy-Item $f -Destination $distDir
    Write-Host "  + $(Split-Path $f -Leaf)"
}

# curl-impersonate is a folder (files must stay together)
Copy-Item $curlDir -Destination $distDir -Recurse
Write-Host "  + curl-impersonate/"

Write-Host "`ndist/ ready: $distDir" -ForegroundColor Green
Get-ChildItem $distDir | Select-Object Name, @{N='Size';E={"{0:N0} KB" -f ($_.Length/1KB)}} | Format-Table -AutoSize

if (-not $WithPdb) {
    $pdb = Join-Path $releaseDir 'hebnix_app.pdb'
    Write-Host "no pdb shipped. archive this per release to read crash.txt later:" -ForegroundColor Yellow
    Write-Host "  $pdb"
}
