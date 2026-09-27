# Builds and tests the PlasmaTrack plugin DLL on Windows with llvm-mingw.
#
# Tools are looked up in this order: -ToolsDir (portable folders as unpacked by
# docs/BUILD.md), then PATH (e.g. winget installs of Kitware.CMake,
# Ninja-build.Ninja, MartinStorsjo.LLVM-MinGW.UCRT).
#
#   powershell -ExecutionPolicy Bypass -File scripts/build-plugin.ps1 [-Config Release] [-NoTest]
param(
    [string]$ToolsDir = (Join-Path $env:LOCALAPPDATA 'PlasmaTrackTools'),
    [string]$Config = 'Release',
    [switch]$NoTest
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

if (Test-Path $ToolsDir) {
    $extra = @()
    $extra += Get-ChildItem $ToolsDir -Directory -Filter 'cmake-*' | ForEach-Object { Join-Path $_.FullName 'bin' }
    $extra += $ToolsDir
    $extra += Get-ChildItem $ToolsDir -Directory -Filter 'llvm-mingw-*ucrt*' | ForEach-Object { Join-Path $_.FullName 'bin' }
    $env:PATH = ($extra -join ';') + ';' + $env:PATH
}

$clang = (Get-Command x86_64-w64-mingw32-clang++ -ErrorAction SilentlyContinue)
if (-not $clang) { throw 'llvm-mingw not found (x86_64-w64-mingw32-clang++). See docs/BUILD.md.' }
$llvmMingwRoot = Split-Path -Parent (Split-Path -Parent $clang.Source)
foreach ($t in 'cmake', 'ninja') {
    if (-not (Get-Command $t -ErrorAction SilentlyContinue)) { throw "$t not found. See docs/BUILD.md." }
}

$build = Join-Path $root 'plugin/build-win'
cmake -S (Join-Path $root 'plugin') -B $build -G Ninja `
    "-DCMAKE_BUILD_TYPE=$Config" `
    "-DCMAKE_TOOLCHAIN_FILE=$(Join-Path $root 'plugin/cmake/toolchain-llvm-mingw.cmake')" `
    "-DLLVM_MINGW_ROOT=$llvmMingwRoot"
if ($LASTEXITCODE) { throw 'cmake configure failed' }
cmake --build $build
if ($LASTEXITCODE) { throw 'build failed' }

if (-not $NoTest) {
    ctest --test-dir $build --output-on-failure
    if ($LASTEXITCODE) { throw 'tests failed' }
}
Write-Host "Plugin: $(Join-Path $build 'plasmatrack.dll')"
