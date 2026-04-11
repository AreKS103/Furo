# Furo Release Script
# Usage:  .\release.ps1             (patch: 0.2.0 -> 0.2.1)
#         .\release.ps1 -Minor      (minor: 0.2.0 -> 0.3.0)
#         .\release.ps1 -Major      (major: 0.2.0 -> 1.0.0)
#         .\release.ps1 -Notes "What's new in this release"

param(
    [switch]$Minor,
    [switch]$Major,
    [string]$Notes = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# ── Pre-flight checks ────────────────────────────────────────────────────────

if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
    Write-Host ""
    Write-Host "  ERROR: GitHub CLI (gh) is not installed." -ForegroundColor Red
    Write-Host "  Run: winget install --id GitHub.cli" -ForegroundColor Yellow
    Write-Host "  Then: gh auth login" -ForegroundColor Yellow
    Write-Host ""
    exit 1
}

# ── Signing keys (paste from Bitwarden each run) ─────────────────────────────

Write-Host ""
Write-Host "  Signing credentials (from Bitwarden)" -ForegroundColor Cyan
Write-Host "  Press Enter to keep existing value if already set." -ForegroundColor DarkGray
Write-Host ""

$inputKey = Read-Host "  Private key (long string)"
if ($inputKey) { $env:TAURI_SIGNING_PRIVATE_KEY = $inputKey }

if (-not $env:TAURI_SIGNING_PRIVATE_KEY) {
    Write-Host "  ERROR: Private key is required. Aborting." -ForegroundColor Red
    exit 1
}

$inputPwd = Read-Host "  Key password (short string, leave blank if none)"
if ($inputPwd) { $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = $inputPwd }

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

# 1. Read current version from tauri.conf.json
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

# 2. Update tauri.conf.json
$confRaw = Get-Content $tauriConf -Raw
$confRaw = $confRaw -replace '"version"\s*:\s*"[^"]+"', "`"version`": `"$next`""
Set-Content $tauriConf $confRaw -NoNewline
Write-Host "  Updated: src-tauri/tauri.conf.json"

# 3. Update package.json
$pkgFile = "package.json"
$pkgRaw = Get-Content $pkgFile -Raw
$pkgRaw = $pkgRaw -replace '"version"\s*:\s*"[^"]+"', "`"version`": `"$next`""
Set-Content $pkgFile $pkgRaw -NoNewline
Write-Host "  Updated: package.json"

# 4. Build
Write-Host ""
Write-Host "  Building... (this takes about 10 minutes)" -ForegroundColor Yellow
Write-Host ""

npx tauri build --bundles nsis

if ($LASTEXITCODE -ne 0) {
    Write-Host ""
    Write-Host "  ERROR: Build failed. Version files were already bumped to $next." -ForegroundColor Red
    exit 1
}

# 5. Show artifact locations
$bundleDir = "C:\Users\alpla\AppData\Local\furo-target\release\bundle"
$nsisDir   = "$bundleDir\nsis"
$installer = Get-ChildItem $nsisDir -Filter "*.exe"     | Select-Object -First 1
$sig       = Get-ChildItem $nsisDir -Filter "*.exe.sig" | Select-Object -First 1
$manifest  = Get-ChildItem $bundleDir -Filter "latest.json" | Select-Object -First 1

if (-not $installer -or -not $sig -or -not $manifest) {
    Write-Host ""
    Write-Host "  ERROR: Could not find all 3 build artifacts." -ForegroundColor Red
    Write-Host "  Expected in: $bundleDir" -ForegroundColor Gray
    exit 1
}

Write-Host ""
Write-Host "  Build complete! v$next" -ForegroundColor Green
Write-Host ""
Write-Host "  Artifacts:" -ForegroundColor White
Write-Host "    $($installer.FullName)" -ForegroundColor Gray
Write-Host "    $($sig.FullName)"       -ForegroundColor Gray
Write-Host "    $($manifest.FullName)"  -ForegroundColor Gray

# 6. Git commit + tag ─────────────────────────────────────────────────────────
Write-Host ""
Write-Host "  Committing version bump..." -ForegroundColor Yellow

$tagName = "v$next"
$releaseNotes = if ($Notes) { $Notes } else { "Furo $tagName" }

git add src-tauri/tauri.conf.json package.json
git commit -m "release: $tagName"
git tag $tagName

# 7. Push to GitHub ───────────────────────────────────────────────────────────
Write-Host "  Pushing to GitHub..." -ForegroundColor Yellow

$branch = git branch --show-current
git push origin $branch
git push origin $tagName

# 8. Create GitHub Release and upload artifacts ───────────────────────────────
Write-Host "  Creating GitHub release $tagName..." -ForegroundColor Yellow

gh release create $tagName `
    "$($installer.FullName)" `
    "$($sig.FullName)" `
    "$($manifest.FullName)" `
    --title "Furo $tagName" `
    --notes $releaseNotes

Write-Host ""
Write-Host "  Released! https://github.com/AreKS103/Furo/releases/tag/$tagName" -ForegroundColor Green
Write-Host "  The app will auto-detect this update within 30 minutes." -ForegroundColor Cyan
Write-Host ""
