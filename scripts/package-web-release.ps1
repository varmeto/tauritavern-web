param(
    [string]$ReleaseTag = '2.3.0-v1.1-arm64',
    [string]$OutputRoot = 'C:\Users\varmeto\OneDrive\for_unas'
)

$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$packageDirectory = Join-Path $OutputRoot ("v1-tauritavern-web-" + $ReleaseTag)
$imageArchive = Join-Path $packageDirectory ("tauritavern-web-" + $ReleaseTag + '.tar')
$sourceArchive = Join-Path $packageDirectory ("tauritavern-web-source-" + $ReleaseTag + '.zip')

if (-not (Test-Path -LiteralPath $OutputRoot -PathType Container)) {
    throw "Output root does not exist: $OutputRoot"
}
if (Test-Path -LiteralPath $packageDirectory) {
    throw "Release directory already exists; use a new release tag: $packageDirectory"
}

Push-Location $projectRoot
try {
    $dirty = @(git status --porcelain)
    if ($LASTEXITCODE -ne 0 -or $dirty.Count -gt 0) {
        throw 'Commit all source changes before packaging, so the source archive matches the images.'
    }
    $sourceRevision = (git rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Unable to read source revision.' }
    New-Item -ItemType Directory -Path $packageDirectory | Out-Null

    docker buildx build --platform linux/arm64 --provenance=false --sbom=false --file web/gateway/Dockerfile --build-arg "TT_WEB_ADAPTER_COMMIT=$sourceRevision" --tag "tauritavern-web-gateway:$ReleaseTag" --load .
    if ($LASTEXITCODE -ne 0) { throw 'ARM64 gateway build failed.' }
    docker buildx build --platform linux/arm64 --provenance=false --sbom=false --file web/app-server/Dockerfile --tag "tauritavern-web-app:$ReleaseTag" --load .
    if ($LASTEXITCODE -ne 0) { throw 'ARM64 application build failed.' }
    docker save --output $imageArchive "tauritavern-web-gateway:$ReleaseTag" "tauritavern-web-app:$ReleaseTag"
    if ($LASTEXITCODE -ne 0) { throw 'Docker image export failed.' }
    git archive --format=zip --output=$sourceArchive HEAD
    if ($LASTEXITCODE -ne 0) { throw 'Source archive export failed.' }

    Copy-Item -LiteralPath 'packaging/dh4300plus-v1/compose.yaml' -Destination $packageDirectory
    Copy-Item -LiteralPath 'packaging/dh4300plus-v1/.env.example' -Destination $packageDirectory
    Copy-Item -LiteralPath 'packaging/dh4300plus-v1/README.zh-CN.md' -Destination $packageDirectory
    Copy-Item -LiteralPath 'LICENSE' -Destination $packageDirectory
    @(
        "Upstream: v2.3.0 / a1855be4a4f8b6ee7cd0374a84dbb3709c3e5375"
        "Web adapter: $sourceRevision"
        "Release tag: $ReleaseTag"
        'License: AGPL-3.0; see LICENSE and matching source archive.'
    ) | Set-Content -LiteralPath (Join-Path $packageDirectory 'SOURCE.txt') -Encoding UTF8

    $packageFiles = Get-ChildItem -LiteralPath $packageDirectory -File | Where-Object Name -ne 'SHA256SUMS.txt'
    $checksums = foreach ($file in $packageFiles) {
        $hash = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        "$hash  $($file.Name)"
    }
    $checksums | Set-Content -LiteralPath (Join-Path $packageDirectory 'SHA256SUMS.txt') -Encoding ASCII
    Write-Output $packageDirectory
} finally {
    Pop-Location
}
