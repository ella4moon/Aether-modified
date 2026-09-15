$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Unmodified upstream distribution. Update version and digest together.
$version = '1.14.1'
$expected = '5197f16d492d93202dc623622149a6ed040f8eca263128f91d603f2b901baa89'
$asset = "sing-box-$version-windows-amd64.zip"
$tempDir = Join-Path ([IO.Path]::GetTempPath()) ([guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $tempDir | Out-Null
try {
    $archive = Join-Path $tempDir $asset
    & curl.exe --fail --location --silent --show-error --retry 3 -o $archive "https://github.com/SagerNet/sing-box/releases/download/v$version/$asset"
    if ($LASTEXITCODE -ne 0) { throw 'Whole-laptop component download failed.' }
    $actual = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $expected) { throw 'Whole-laptop component checksum mismatch.' }
    Expand-Archive -LiteralPath $archive -DestinationPath $tempDir
    $source = Join-Path $tempDir "sing-box-$version-windows-amd64"
    Copy-Item -LiteralPath (Join-Path $source 'sing-box.exe') -Destination $PSScriptRoot
    Copy-Item -LiteralPath (Join-Path $source 'libcronet.dll') -Destination $PSScriptRoot
    Copy-Item -LiteralPath (Join-Path $source 'LICENSE') -Destination (Join-Path $PSScriptRoot 'sing-box-LICENSE.txt')
} finally {
    Remove-Item -LiteralPath $tempDir -Recurse -Force
}
