<#
install.ps1 — one-shot installer for openproxy on Windows.

Usage:
  irm https://raw.githubusercontent.com/quangdang46/openproxy/main/install.ps1 | iex
  iwr https://raw.githubusercontent.com/quangdang46/openproxy/main/install.ps1 -UseBasicParsing | iex

Pinning a version or passing flags through `irm | iex` requires a small wrapper:
  & ([scriptblock]::Create((irm 'https://raw.githubusercontent.com/quangdang46/openproxy/main/install.ps1'))) -Version v0.1.7 -EasyMode

Or download and run directly:
  irm https://raw.githubusercontent.com/quangdang46/openproxy/main/install.ps1 -OutFile install.ps1
  .\install.ps1 -Version v0.1.7 -EasyMode

Flags:
  -Dest <path>          Install location. Default: $env:USERPROFILE\.local\bin
  -System               Shortcut for -Dest "$env:ProgramFiles\openproxy" (admin)
  -Version <vX.Y.Z>     Pin a specific release. Default: latest
  -EasyMode             Append the install dir to the *user* PATH if missing
  -Verify               Run `openproxy --version` after install
  -NoSkill              Skip installing the agent skill into the user profile
  -SkillDest <dir>      Override the skills root. Default: $env:USERPROFILE\.agents\skills
  -Quiet                Suppress info logs
  -Uninstall            Remove the binary and any easy-mode PATH entry
  -Help                 Show this help and exit
#>

[CmdletBinding()]
param(
    [string] $Dest      = "$env:USERPROFILE\.local\bin",
    [switch] $System,
    [string] $Version   = "",
    [switch] $EasyMode,
    [switch] $Verify,
    [switch] $NoSkill,
    [string] $SkillDest = "$env:USERPROFILE\.agents\skills",
    [switch] $Quiet,
    [switch] $Uninstall,
    [switch] $Help
)

$ErrorActionPreference = 'Stop'
$ProgressPreference    = 'SilentlyContinue'   # Disables the slow IE-style progress bar in Invoke-WebRequest.

# ════════════════════════════════════════════════════════════════════════════
# Configuration
# ════════════════════════════════════════════════════════════════════════════

$BinaryName = 'openproxy'
$BinaryFile = "$BinaryName.exe"
$Owner      = 'quangdang46'
$Repo       = 'openproxy'

if ($System) { $Dest = "$env:ProgramFiles\$BinaryName" }

# ════════════════════════════════════════════════════════════════════════════
# Logging
# ════════════════════════════════════════════════════════════════════════════

function Write-Info  { param($msg) if (-not $Quiet) { Write-Host "==> [$BinaryName] $msg" -ForegroundColor Cyan } }
function Write-Warn  { param($msg) Write-Host "!! [$BinaryName] $msg" -ForegroundColor Yellow }
function Write-Ok    { param($msg) if (-not $Quiet) { Write-Host "✓ $msg" -ForegroundColor Green } }
function Die         { param($msg) Write-Host "ERROR: $msg" -ForegroundColor Red; exit 1 }

# ════════════════════════════════════════════════════════════════════════════
# Help
# ════════════════════════════════════════════════════════════════════════════

if ($Help) {
    # Print everything between the first <# and the matching #>.
    $self = $MyInvocation.MyCommand.Path
    if (-not $self) { $self = $PSCommandPath }
    if ($self -and (Test-Path $self)) {
        $content = Get-Content -Raw $self
        if ($content -match '(?s)<#(.*?)#>') { Write-Host $matches[1].Trim() }
    } else {
        Write-Host "openproxy installer for Windows. Run with -Help on a downloaded copy for full text."
    }
    exit 0
}

# ════════════════════════════════════════════════════════════════════════════
# Platform detection — Windows only. Anything else: bail with a hint.
# ════════════════════════════════════════════════════════════════════════════

function Get-Platform {
    if ($IsLinux -or $IsMacOS) {
        Die "install.ps1 is for Windows only. On Linux / macOS use install.sh:`n  curl -fsSL https://raw.githubusercontent.com/$Owner/$Repo/main/install.sh | bash"
    }
    $arch = $env:PROCESSOR_ARCHITECTURE
    # WOW64 reports x86 even on 64-bit; check PROCESSOR_ARCHITEW6432 too.
    if ($env:PROCESSOR_ARCHITEW6432) { $arch = $env:PROCESSOR_ARCHITEW6432 }
    switch -Wildcard ($arch) {
        'AMD64' { return 'windows-x86_64' }
        'x86_64'{ return 'windows-x86_64' }
        'ARM64' { Die "Windows on ARM64 isn't published yet. Track https://github.com/$Owner/$Repo/issues for updates." }
        default { Die "unsupported architecture: $arch" }
    }
}

# ════════════════════════════════════════════════════════════════════════════
# Uninstall
# ════════════════════════════════════════════════════════════════════════════

function Invoke-Uninstall {
    $target = Join-Path $Dest $BinaryFile
    if (Test-Path $target) {
        Remove-Item -LiteralPath $target -Force
        Write-Ok "removed $target"
    } else {
        Write-Warn "no binary at $target"
    }

    # Strip $Dest from the user PATH if we ever appended it.
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ($userPath) {
        $entries = $userPath -split ';' | Where-Object { $_ -and ($_ -ne $Dest) }
        $newPath = ($entries -join ';')
        if ($newPath -ne $userPath) {
            [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
            Write-Ok "removed $Dest from user PATH"
        }
    }

    # Remove the auto-installed agent skill if it still has our marker.
    $skillFile = Join-Path (Join-Path $SkillDest $BinaryName) 'SKILL.md'
    if (Test-Path $skillFile) {
        $first = Get-Content -LiteralPath $skillFile -TotalCount 5 -ErrorAction SilentlyContinue
        if ($first -match "^name:\s+$BinaryName\s*$") {
            Remove-Item -LiteralPath $skillFile -Force
            Remove-Item -LiteralPath (Split-Path $skillFile) -Force -ErrorAction SilentlyContinue
            Write-Ok "removed agent skill $skillFile"
        }
    }

    Write-Ok "uninstalled"
    exit 0
}

if ($Uninstall) { Invoke-Uninstall }

# ════════════════════════════════════════════════════════════════════════════
# Version resolution
# ════════════════════════════════════════════════════════════════════════════

function Resolve-Version {
    if ($script:Version) {
        if (-not $script:Version.StartsWith('v')) { $script:Version = "v$script:Version" }
        return
    }

    # Primary: GitHub releases API.
    try {
        $api = "https://api.github.com/repos/$Owner/$Repo/releases/latest"
        $resp = Invoke-RestMethod -Uri $api -Headers @{ 'Accept' = 'application/vnd.github.v3+json' } -TimeoutSec 30
        if ($resp.tag_name) { $script:Version = $resp.tag_name; Write-Info "latest version: $script:Version"; return }
    } catch {
        Write-Warn "GitHub API request failed; falling back to redirect probe ($($_.Exception.Message))"
    }

    # Fallback: HEAD the /releases/latest page and read the redirect target.
    try {
        $resp = Invoke-WebRequest -Uri "https://github.com/$Owner/$Repo/releases/latest" -MaximumRedirection 0 -ErrorAction SilentlyContinue
        $loc  = $resp.Headers.Location
        if ($loc -and $loc -match '/tag/(v[0-9][^/?#]*)') {
            $script:Version = $matches[1]
            Write-Info "latest version: $script:Version"
            return
        }
    } catch { }

    Die "could not resolve latest version. Pass -Version vX.Y.Z to pin."
}

# ════════════════════════════════════════════════════════════════════════════
# Download with retry
# ════════════════════════════════════════════════════════════════════════════

function Download-File {
    param(
        [Parameter(Mandatory)] [string] $Url,
        [Parameter(Mandatory)] [string] $OutPath,
        [int]    $MaxRetries = 3,
        [int]    $TimeoutSec = 120
    )
    for ($attempt = 1; $attempt -le $MaxRetries; $attempt++) {
        try {
            Invoke-WebRequest -Uri $Url -OutFile $OutPath -TimeoutSec $TimeoutSec -UseBasicParsing
            return $true
        } catch {
            if ($attempt -lt $MaxRetries) {
                Write-Warn "download attempt $attempt failed; retrying in 3s..."
                Start-Sleep -Seconds 3
            } else {
                Write-Warn "download failed: $($_.Exception.Message)"
                return $false
            }
        }
    }
    return $false
}

# ════════════════════════════════════════════════════════════════════════════
# PATH update (opt-in via -EasyMode)
# ════════════════════════════════════════════════════════════════════════════

function Update-UserPath {
    $current = $env:Path -split ';'
    if ($current -contains $Dest) { return }

    if ($EasyMode) {
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $entries  = if ($userPath) { $userPath -split ';' } else { @() }
        if ($entries -notcontains $Dest) {
            $newPath = (($entries + $Dest) | Where-Object { $_ } ) -join ';'
            [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
            Write-Ok "added $Dest to user PATH"
            Write-Warn "open a new PowerShell window for the change to take effect."
        }
    } else {
        Write-Warn "$Dest is not on your PATH. Either:"
        Write-Warn "  - rerun with -EasyMode to add it permanently to your user PATH, or"
        Write-Warn "  - prepend it manually:  `$env:Path = '$Dest;' + `$env:Path"
    }
}

# ════════════════════════════════════════════════════════════════════════════
# Agent skill install
#
# Drop SKILL.md at $SkillDest\openproxy\SKILL.md so agents that auto-discover
# the skills root (Devin, Claude Code, …) can install + drive openproxy on
# the user's behalf. Idempotent: preserves user-edited files.
# ════════════════════════════════════════════════════════════════════════════

function Install-AgentSkill {
    if ($NoSkill) { return }
    $skillDir  = Join-Path $SkillDest $BinaryName
    $skillFile = Join-Path $skillDir 'SKILL.md'

    if (Test-Path $skillFile) {
        $head = Get-Content -LiteralPath $skillFile -TotalCount 5 -ErrorAction SilentlyContinue
        if (-not ($head -match "^name:\s+$BinaryName\s*$")) {
            Write-Info "agent skill at $skillFile looks user-edited — leaving it alone"
            return
        }
    }

    try { New-Item -ItemType Directory -Force -Path $skillDir | Out-Null }
    catch { Write-Warn "could not create $skillDir — skipping agent skill install"; return }

    $ref = if ($script:Version) { $script:Version } else { 'main' }
    $url = "https://raw.githubusercontent.com/$Owner/$Repo/$ref/.agents/skills/$BinaryName/SKILL.md"
    $tmp = "$skillFile.tmp.$PID"
    if (Download-File -Url $url -OutPath $tmp -MaxRetries 2 -TimeoutSec 30) {
        Move-Item -LiteralPath $tmp -Destination $skillFile -Force
        Write-Ok "agent skill installed → $skillFile"
    } else {
        Remove-Item -LiteralPath $tmp -ErrorAction SilentlyContinue
        Write-Warn "could not download agent skill from $url (continuing)"
    }
}

# ════════════════════════════════════════════════════════════════════════════
# Atomic install — write to a temp file in the destination dir, then move.
# ════════════════════════════════════════════════════════════════════════════

function Install-Binary-Atomic {
    param([string] $SourcePath, [string] $DestPath)
    $tmp = "$DestPath.tmp.$PID"
    Copy-Item -LiteralPath $SourcePath -Destination $tmp -Force
    try {
        Move-Item -LiteralPath $tmp -Destination $DestPath -Force
    } catch {
        Remove-Item -LiteralPath $tmp -ErrorAction SilentlyContinue
        Die "failed to write $DestPath ($($_.Exception.Message))"
    }
}

# ════════════════════════════════════════════════════════════════════════════
# Main
# ════════════════════════════════════════════════════════════════════════════

$tempDir = Join-Path $env:TEMP "openproxy-install-$PID"
New-Item -ItemType Directory -Force -Path $tempDir | Out-Null

try {
    if (-not (Test-Path $Dest)) { New-Item -ItemType Directory -Force -Path $Dest | Out-Null }

    $platform = Get-Platform
    Write-Info "platform: $platform"
    Write-Info "destination: $Dest"

    Resolve-Version

    $archive = "$BinaryName-$Version-$platform.zip"
    $base    = "https://github.com/$Owner/$Repo/releases/download/$Version"
    $archivePath = Join-Path $tempDir $archive

    Write-Info "downloading $archive"
    if (-not (Download-File -Url "$base/$archive" -OutPath $archivePath)) {
        Die @"
failed to download $archive

Windows binaries are published starting from a future release; the version you
asked for ($Version) does not include $archive. Either:
  - pin a release that does:   irm <url> | iex with -Version v0.1.7 (or newer)
  - or build from source:       https://github.com/$Owner/$Repo#build-from-source
"@
    }

    # Verify SHA-256 if the sidecar exists.
    $sumPath = "$archivePath.sha256"
    if (Download-File -Url "$base/$archive.sha256" -OutPath $sumPath -MaxRetries 1 -TimeoutSec 30) {
        $expected = (Get-Content -LiteralPath $sumPath -Raw).Trim().Split()[0]
        $actual   = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLower()
        if ($expected.ToLower() -ne $actual) {
            Die "checksum mismatch for $archive — expected $expected, got $actual"
        }
        Write-Info "checksum verified"
    } else {
        Write-Warn "no checksum file at $archive.sha256 — skipping verification"
    }

    # Extract.
    $extractDir = Join-Path $tempDir 'extract'
    Expand-Archive -LiteralPath $archivePath -DestinationPath $extractDir -Force

    # Locate openproxy.exe inside the archive (top level, but tolerate one level).
    $bin = Get-ChildItem -LiteralPath $extractDir -Recurse -Filter $BinaryFile -File |
           Select-Object -First 1
    if (-not $bin) { Die "$BinaryFile not found inside $archive" }

    Install-Binary-Atomic -SourcePath $bin.FullName -DestPath (Join-Path $Dest $BinaryFile)

    Update-UserPath
    Install-AgentSkill

    if ($Verify) {
        Write-Info "running self-test: $Dest\$BinaryFile --version"
        & (Join-Path $Dest $BinaryFile) --version | Out-Host
    }

    Write-Host ""
    Write-Host "✓ $BinaryName installed → $(Join-Path $Dest $BinaryFile)" -ForegroundColor Green
    try {
        $v = & (Join-Path $Dest $BinaryFile) --version 2>$null
        if ($v) { Write-Host "   version: $v" }
    } catch { }
    Write-Host ""
    Write-Host "   start the server + dashboard:"
    Write-Host "     $BinaryName"
    Write-Host "   then visit:    http://127.0.0.1:4623/"
    Write-Host "   full help:     $BinaryName --help"
    Write-Host "   uninstall:     irm https://raw.githubusercontent.com/$Owner/$Repo/main/install.ps1 -OutFile `$env:TEMP\op-uninstall.ps1; & `$env:TEMP\op-uninstall.ps1 -Uninstall"
    Write-Host ""
}
finally {
    if (Test-Path $tempDir) { Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue }
}
