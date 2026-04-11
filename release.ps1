# Furo Release Script
#
# Two workflows:
#   npm run release           -- Local build only (test .exe, no version bump, no git)
#   npm run release:ci        -- Bump patch, commit, tag, push → GitHub Actions builds
#   npm run release:ci:minor  -- Same, minor bump
#   npm run release:ci:major  -- Same, major bump

param(
    [switch]$Minor,
    [switch]$Major,
    [switch]$CI,
    [string]$Notes = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# ══════════════════════════════════════════════════════════════════════════════
# MODE 1: Local build for testing (npm run release)
# ══════════════════════════════════════════════════════════════════════════════
if (-not $CI) {
    Write-Host ""
    Write-Host "  Local build mode -- compiling .exe for testing" -ForegroundColor Cyan
    Write-Host "  (No version bump, no git push, no GitHub release)" -ForegroundColor DarkGray
    Write-Host ""

    # Build the app (Tauri + Vite + Rust)
    # Disable createUpdaterArtifacts so no signing key or password is required for local testing.
    Write-Host ""
    Write-Host "  Building... (this takes about 10 minutes)" -ForegroundColor Yellow
    Write-Host ""

    # Write config to a temp file to avoid PowerShell quote-stripping issues with --config JSON inline
    $tmpConfig = [System.IO.Path]::GetTempFileName() + ".json"
    '{"bundle":{"createUpdaterArtifacts":false}}' | Set-Content $tmpConfig -Encoding utf8

    npx tauri build --bundles nsis --config $tmpConfig

    Remove-Item $tmpConfig -ErrorAction SilentlyContinue

    if ($LASTEXITCODE -ne 0) {
        Write-Host ""
        Write-Host "  ERROR: Build failed." -ForegroundColor Red
        exit 1
    }

    # Locate the built artifacts
    $tauriConf = "src-tauri\tauri.conf.json"
    $conf = Get-Content $tauriConf -Raw | ConvertFrom-Json
    $version = $conf.version

    $bundleDir = "C:\Users\alpla\AppData\Local\furo-target\release\bundle"
    $nsisDir   = "$bundleDir\nsis"
    $installer = Get-ChildItem $nsisDir -Filter "Furo_${version}_x64-setup.exe" | Select-Object -First 1

    Write-Host ""
    Write-Host "  Build complete! v$version" -ForegroundColor Green
    Write-Host ""
    if ($installer) {
        Write-Host "  Installer: $($installer.FullName)" -ForegroundColor Gray
        Write-Host ""
        Write-Host "  Run the .exe to test. When ready to deploy:" -ForegroundColor White
        Write-Host "    npm run release:ci" -ForegroundColor Yellow
    } else {
        Write-Host "  Artifacts dir: $nsisDir" -ForegroundColor Gray
    }
    Write-Host ""
    exit 0
}

# ══════════════════════════════════════════════════════════════════════════════
# MODE 2: CI deploy (npm run release:ci)
# ══════════════════════════════════════════════════════════════════════════════

Write-Host ""
Write-Host "  CI mode: bump version, tag, and push. GitHub Actions will build." -ForegroundColor Cyan
Write-Host ""

# ── Pre-flight checks ────────────────────────────────────────────────────────

if (-not (Test-Path ".git")) {
    Write-Host ""
    Write-Host "  ERROR: Not a git repository." -ForegroundColor Red
    Write-Host "  Run the one-time setup:" -ForegroundColor Yellow
    Write-Host "    git init" -ForegroundColor Gray
    Write-Host "    git remote add origin https://github.com/AreKS103/Furo.git" -ForegroundColor Gray
    Write-Host "    git add ." -ForegroundColor Gray
    Write-Host "    git commit -m `"initial commit`"" -ForegroundColor Gray
    Write-Host "    git branch -M main" -ForegroundColor Gray
    Write-Host "    git push -u origin main" -ForegroundColor Gray
    Write-Host ""
    exit 1
}

$uncommitted = git status --porcelain | Where-Object { $_ -notmatch "^\?\?" }
if ($uncommitted) {
    Write-Host ""
    Write-Host "  Committing pending changes before release..." -ForegroundColor Yellow
    git add -A
    git commit -m "chore: pre-release cleanup"
}

# 1. Read current version from tauri.conf.json ────────────────────────────────
$tauriConf = "src-tauri\tauri.conf.json"
$conf = Get-Content $tauriConf -Raw | ConvertFrom-Json
$current = $conf.version

$parts = $current -split '\.' | ForEach-Object { [int]$_ }
$maj, $min, $pat = $parts[0], $parts[1], $parts[2]

if ($Major) {
    $maj++; $min = 0; $pat = 0
} elseif ($Minor) {
    $min++; $pat = 0
} else {
    $pat++
}

$next = "$maj.$min.$pat"

Write-Host ""
Write-Host "  Version: $current -> $next" -ForegroundColor Cyan
Write-Host ""

# 2. Update tauri.conf.json ───────────────────────────────────────────────────
$confRaw = Get-Content $tauriConf -Raw
$confRaw = $confRaw -replace '"version"\s*:\s*"[^"]+"', "`"version`": `"$next`""
Set-Content $tauriConf $confRaw -NoNewline
Write-Host "  Updated: src-tauri/tauri.conf.json"

# 3. Update package.json ─────────────────────────────────────────────────────
$pkgFile = "package.json"
$pkgRaw = Get-Content $pkgFile -Raw
$pkgRaw = $pkgRaw -replace '"version"\s*:\s*"[^"]+"', "`"version`": `"$next`""
Set-Content $pkgFile $pkgRaw -NoNewline
Write-Host "  Updated: package.json"

# 4. Update Cargo.toml ────────────────────────────────────────────────────────
$cargoFile = "src-tauri\Cargo.toml"
$cargoRaw = Get-Content $cargoFile -Raw
# Only replace the first occurrence (the [package] version, not dependency versions)
$cargoRaw = $cargoRaw -replace '(?m)^version = "[^"]+"', "version = `"$next`""
Set-Content $cargoFile $cargoRaw -NoNewline
Write-Host "  Updated: src-tauri/Cargo.toml"

# 5. Git commit + tag ─────────────────────────────────────────────────────────
Write-Host ""
Write-Host "  Committing version bump..." -ForegroundColor Yellow

$tagName = "v$next"

git add src-tauri/tauri.conf.json package.json src-tauri/Cargo.toml
git commit -m "release: $tagName"
git tag $tagName

# 5. Push to GitHub ───────────────────────────────────────────────────────────
Write-Host "  Pushing to GitHub..." -ForegroundColor Yellow

$branch = git branch --show-current
git push origin $branch
git push origin $tagName

Write-Host ""
Write-Host "  Tag $tagName pushed. GitHub Actions will build and release." -ForegroundColor Green
Write-Host "  Monitor: https://github.com/AreKS103/Furo/actions" -ForegroundColor Cyan
Write-Host ""
