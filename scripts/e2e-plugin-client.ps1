# End-to-end on Windows: the REAL plugin DLL (driven through a scripted job by
# pt_drive_plugin) talks to the REAL plasmatrack-client binary over loopback.
# Checks that every frame is persisted by the client and acknowledged back to the
# plugin's journal.
#
#   powershell -ExecutionPolicy Bypass -File scripts/e2e-plugin-client.ps1
param([int]$Port = 30555)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$tools = Join-Path $env:LOCALAPPDATA 'PlasmaTrackTools'
if (Test-Path $tools) {
    $extra = @(Get-ChildItem $tools -Directory -Filter 'cmake-*' | ForEach-Object { Join-Path $_.FullName 'bin' }) + $tools +
             @(Get-ChildItem $tools -Directory -Filter 'llvm-mingw-*ucrt*' | ForEach-Object { Join-Path $_.FullName 'bin' })
    $env:PATH = ($extra -join ';') + ";$env:USERPROFILE\.cargo\bin;" + $env:PATH
}

& powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root 'scripts/build-plugin.ps1') -NoTest | Out-Null
if ($LASTEXITCODE) { throw 'plugin build failed' }
Push-Location $root
cargo build -q -p pt-client
if ($LASTEXITCODE) { throw 'client build failed' }
Pop-Location

$client = Join-Path $root 'target/debug/plasmatrack-client.exe'
$driver = Join-Path $root 'plugin/build-win/pt_drive_plugin.exe'
$dll = Join-Path $root 'plugin/build-win/plasmatrack.dll'
$tmp = Join-Path $env:TEMP ("pt-e2e-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force $tmp | Out-Null
$cfgPath = Join-Path $tmp 'plasmatrack.toml'
(Get-Content (Join-Path $root 'config/plasmatrack.sample.toml') -Raw) `
    -replace '(?m)^port = 30455', "port = $Port" `
    -replace '(?m)^status_port = 30456', "status_port = $($Port + 1)" |
    Set-Content -NoNewline -Encoding ascii $cfgPath

$data = Join-Path $tmp 'data'
$log = Join-Path $tmp 'client.log'
$proc = Start-Process -FilePath $client -ArgumentList @('run', '--config', $cfgPath, '--data-dir', $data) `
    -PassThru -NoNewWindow -RedirectStandardError $log
try {
    # Wait until the client listens.
    $deadline = (Get-Date).AddSeconds(15)
    while ($true) {
        try { (New-Object Net.Sockets.TcpClient('127.0.0.1', $Port)).Close(); break } catch {}
        if ((Get-Date) -gt $deadline) { throw "client did not start: $(Get-Content $log -Raw)" }
        Start-Sleep -Milliseconds 100
    }
    $sess = (& $driver $dll (Join-Path $tmp 'plugins') $Port) | Select-Object -Last 1
    if ($LASTEXITCODE -or -not $sess) { throw 'driver failed' }
    Start-Sleep -Milliseconds 500
    $status = Invoke-RestMethod "http://127.0.0.1:$($Port + 1)/status.json"
} finally {
    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
}

$stats = (& $client status --data-dir $data) | ConvertFrom-Json
$ackFile = Join-Path $tmp "plugins/plasmatrack/journal/$sess.ack"
$acked = if (Test-Path $ackFile) { [uint64](Get-Content $ackFile -Raw) } else { 0 }
$fail = @()
if ($stats.sessions -ne 1) { $fail += "expected 1 session, got $($stats.sessions)" }
if ($stats.frames -lt 65) { $fail += "expected >= 65 frames (3 s at 20 Hz + events), got $($stats.frames)" }
if ($stats.snapshots -lt 1) { $fail += 'no snapshot stored' }
if ($acked -lt 60) { $fail += "plugin journal acknowledged only through seq $acked" }
if ($status.last_game -ne 'ats') { $fail += "status page game: $($status.last_game)" }
if ($status.frames_received -lt $stats.frames) { $fail += 'status page frame count too low' }

Write-Host "session $sess : $($stats.frames) frames stored, plugin journal acked through $acked, game $($status.last_game)"
if ($fail) { $fail | ForEach-Object { Write-Host "FAIL: $_" }; Get-Content $log | Write-Host; exit 1 }
Remove-Item -Recurse -Force $tmp
Write-Host 'e2e plugin <-> client: PASS'
