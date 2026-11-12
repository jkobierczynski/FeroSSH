# Writes the winget manifests for a release, ready to submit to
# https://github.com/microsoft/winget-pkgs (for example with
# `wingetcreate submit <folder>`, or as a pull request by hand).
#
#   pwsh scripts/winget-manifest.ps1 -Version 1.0.0 -Sums dist/SHA256SUMS.txt
#
# The release workflow runs it and uploads the result as the
# winget-manifests artifact. winget installs ferossh.exe (ferossh-arm64.exe
# on Windows on ARM) as a portable app: it downloads the exe, checks its
# SHA-256 and puts a `ferossh` command on the PATH.

param(
    [Parameter(Mandatory)] [string]$Version,
    # SHA256SUMS.txt of the release, as published.
    [Parameter(Mandatory)] [string]$Sums,
    [string]$OutDir = "winget",
    [string]$Repository = "jkobierczynski/FeroSSH"
)

$ErrorActionPreference = "Stop"
$id = "jkobierczynski.FeroSSH"
$schema = "1.9.0"

$sha = $null
$shaArm = $null
foreach ($line in Get-Content $Sums) {
    $hash, $name = $line -split "\s+", 2
    if ($name -eq "ferossh.exe") { $sha = $hash.ToUpper() }
    if ($name -eq "ferossh-arm64.exe") { $shaArm = $hash.ToUpper() }
}
if (-not $sha) { throw "ferossh.exe is not listed in $Sums" }

$repoUrl = "https://github.com/$Repository"
$dir = [IO.Path]::Combine((Get-Location).Path, $OutDir, "manifests", "j", "jkobierczynski", "FeroSSH", $Version)
New-Item -ItemType Directory -Force $dir | Out-Null

function Write-Manifest($file, $type, $body) {
    $text = "# yaml-language-server: `$schema=https://aka.ms/winget-manifest.$type.$schema.schema.json`n`n" +
        "PackageIdentifier: $id`nPackageVersion: $Version`n" + $body +
        "ManifestType: $type`nManifestVersion: $schema`n"
    [IO.File]::WriteAllText((Join-Path $dir $file), $text, [Text.UTF8Encoding]::new($false))
}

Write-Manifest "$id.yaml" "version" @"
DefaultLocale: en-US

"@

# Windows on ARM gets the native ferossh-arm64.exe when the release has
# one; winget installs it under the same `ferossh` command.
$installers = @"
- Architecture: x64
  InstallerUrl: $repoUrl/releases/download/v$Version/ferossh.exe
  InstallerSha256: $sha

"@
if ($shaArm) {
    $installers += @"
- Architecture: arm64
  InstallerUrl: $repoUrl/releases/download/v$Version/ferossh-arm64.exe
  InstallerSha256: $shaArm

"@
}

Write-Manifest "$id.installer.yaml" "installer" @"
MinimumOSVersion: 10.0.18362.0
InstallerType: portable
Commands:
- ferossh
ReleaseDate: $(Get-Date -Format yyyy-MM-dd)
Installers:
$installers
"@

Write-Manifest "$id.locale.en-US.yaml" "defaultLocale" @"
PackageLocale: en-US
Publisher: Jurgen Kobierczynski
PublisherUrl: https://github.com/jkobierczynski
PublisherSupportUrl: $repoUrl/issues
Author: Jurgen Kobierczynski
PackageName: FeroSSH
PackageUrl: $repoUrl
License: MIT OR Apache-2.0
LicenseUrl: $repoUrl#licence
ShortDescription: A small, sandboxed SSH client for Windows with tabs, splits, tmux integration and file transfer.
Description: |-
  FeroSSH is a native, open-source SSH client for Windows, written in Rust. It replaces PuTTY with tabs, splits, a connection manager, tmux control mode, SFTP file transfer, port forwarding, FIDO2 security keys and remote desktops over SSH, in one small exe. The SSH worker runs in an AppContainer sandbox that cannot read your files or keys.
Moniker: ferossh
Tags:
- ssh
- ssh-client
- sftp
- terminal
- tmux
- putty
ReleaseNotesUrl: $repoUrl/releases/tag/v$Version

"@

Get-ChildItem $dir | ForEach-Object { "== $($_.Name)"; Get-Content $_.FullName }
