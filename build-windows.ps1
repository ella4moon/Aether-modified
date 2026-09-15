$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Set-Location $PSScriptRoot

foreach ($tool in @('node', 'npm', 'cargo', 'rustc', 'curl.exe')) {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
        throw "Missing $tool. Follow the prerequisites in FINAL-PROXY.md."
    }
}

$pin = (Select-String -Path 'src-tauri/binaries/fetch-aether.sh' -Pattern 'AETHER_VERSION="(.+)"').Matches[0].Groups[1].Value
$base = "https://github.com/CluvexStudio/Aether/releases/download/$pin"
$tempDir = Join-Path ([IO.Path]::GetTempPath()) ([guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $tempDir | Out-Null
try {
    $archive = Join-Path $tempDir 'aether-windows-x86_64.zip'
    $sums = Join-Path $tempDir 'SHA256SUMS.txt'
    & curl.exe --fail --location --silent --show-error -o $archive "$base/aether-windows-x86_64.zip"
    if ($LASTEXITCODE -ne 0) { throw 'Aether download failed.' }
    & curl.exe --fail --location --silent --show-error -o $sums "$base/SHA256SUMS.txt"
    if ($LASTEXITCODE -ne 0) { throw 'Checksum download failed.' }
    $entry = @(Get-Content $sums | Where-Object { $_ -match '^([a-fA-F0-9]{64})\s+\*?aether-windows-x86_64\.zip$' })
    if ($entry.Count -ne 1) { throw 'The checksum file did not identify one Windows archive.' }
    $expected = ($entry[0] -split '\s+')[0].ToLowerInvariant()
    $actual = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $expected) { throw 'Aether archive checksum mismatch.' }
    $expanded = Join-Path $tempDir 'expanded'
    Expand-Archive -LiteralPath $archive -DestinationPath $expanded
    Copy-Item -LiteralPath (Join-Path $expanded 'aether.exe') -Destination 'src-tauri/binaries/aether.exe'
} finally {
    Remove-Item -LiteralPath $tempDir -Recurse -Force
}

& npm ci
if ($LASTEXITCODE -ne 0) { throw 'Dependency installation failed.' }
& npm run build
if ($LASTEXITCODE -ne 0) { throw 'Frontend build failed.' }
& npm run lint
if ($LASTEXITCODE -ne 0) { throw 'Frontend lint failed.' }
& cargo test --manifest-path src-tauri/exit-proxy/Cargo.toml
if ($LASTEXITCODE -ne 0) { throw 'Final-proxy tests failed.' }
& cargo test --manifest-path src-tauri/Cargo.toml
if ($LASTEXITCODE -ne 0) { throw 'Backend tests failed.' }
& npm run tauri build
if ($LASTEXITCODE -ne 0) { throw 'Desktop build failed.' }
Write-Host 'Build complete. Installers are in src-tauri/target/release/bundle/.'
