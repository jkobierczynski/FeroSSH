# Signs a draft release's exes on this PC and publishes the release.
#
#   pwsh scripts/sign-release.ps1 -Tag v1.0.0-rc2
#
# For releases made with the LOCAL_SIGNING repository variable set: the
# release workflow then leaves a draft release with the unsigned exes. This
# script, run from a checkout of the repository:
#
#   1. downloads ferossh.exe, ferossh-desktop.exe and BUILD-MANIFEST.json
#      from the draft, and checks the exes are the ones CI built;
#   2. signs both exes with signtool and the code signing certificate whose
#      subject contains -Subject (a Certum card, or SimplySign Desktop
#      logged in), with an RFC 3161 timestamp from Certum;
#   3. writes SHA256SUMS.txt and BUILD-MANIFEST.json for the signed exes
#      and uploads all four files to the draft;
#   4. runs the "Attest a signed release" workflow on them and waits;
#   5. names the signer in the release notes, publishes the release and
#      writes the winget manifests for the signed ferossh.exe.
#
# It needs the GitHub CLI (gh, logged in with write access) and signtool
# from the Windows SDK. Nothing is published until every check passes; run
# it again after a failure.

param(
    [Parameter(Mandatory)] [string]$Tag,
    # Part of the certificate's subject; Certum's open source certificates
    # read "Open Source Developer, <your name>".
    [string]$Subject = "Open Source Developer",
    [string]$Timestamp = "http://time.certum.pl",
    [string]$Repository = "jkobierczynski/FeroSSH"
)

$ErrorActionPreference = "Stop"
# gh writes UTF-8; without this PowerShell reads its output in the console's
# code page and the notes come back with "⚠" turned into "ΓÜá".
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$exes = "ferossh.exe", "ferossh-desktop.exe"

function Invoke-Native {
    param([string]$Program, [string[]]$Arguments)
    & $Program $Arguments
    if ($LASTEXITCODE) { throw "$Program $($Arguments[0]) failed with exit code $LASTEXITCODE" }
}

function Find-SignTool {
    $found = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($found) { return $found.Source }
    $kits = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
    $tool = Get-ChildItem $kits -Filter signtool.exe -Recurse -ErrorAction SilentlyContinue |
        Where-Object { $_.Directory.Name -eq "x64" } |
        Sort-Object { $_.Directory.Parent.Name } |
        Select-Object -Last 1
    if (-not $tool) { throw "signtool.exe not found: install the Windows SDK (its Signing Tools are enough)" }
    $tool.FullName
}

if (-not (Get-Command gh -ErrorAction SilentlyContinue)) { throw "the GitHub CLI (gh) is not installed" }
$signtool = Find-SignTool

$release = gh release view $Tag -R $Repository --json isDraft,body | ConvertFrom-Json
if ($LASTEXITCODE) { throw "release $Tag not found in $Repository" }
if (-not $release.isDraft) { throw "release $Tag is already published; this script only signs drafts" }

$work = Join-Path ([IO.Path]::GetTempPath()) "ferossh-sign-$Tag"
Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $work | Out-Null
Write-Host "Downloading the draft's files to $work"
Invoke-Native gh @("release", "download", $Tag, "-R", $Repository, "-D", $work,
    "-p", "ferossh.exe", "-p", "ferossh-desktop.exe", "-p", "BUILD-MANIFEST.json")

# Sign only what CI built: the manifest has each exe's SHA-256 before
# signing, the hash a rebuild from the tag reproduces.
$manifest = Get-Content -Raw (Join-Path $work "BUILD-MANIFEST.json") | ConvertFrom-Json
foreach ($exe in $exes) {
    $entry = $manifest.files | Where-Object name -eq $exe
    $hash = (Get-FileHash (Join-Path $work $exe) -Algorithm SHA256).Hash.ToLower()
    if (-not $entry -or $hash -ne $entry.sha256_unsigned) {
        throw "$exe does not match the SHA-256 the release workflow built ($($entry.sha256_unsigned)); not signing it"
    }
}

Write-Host "Signing with the certificate for '$Subject' (SimplySign or the card may ask for your PIN or a code)"
foreach ($exe in $exes) {
    Invoke-Native $signtool @("sign", "/n", $Subject, "/fd", "sha256", "/tr", $Timestamp, "/td", "sha256",
        "/d", "FeroSSH", "/du", "https://github.com/$Repository", (Join-Path $work $exe))
}
$signer = $null
foreach ($exe in $exes) {
    $sig = Get-AuthenticodeSignature (Join-Path $work $exe)
    if ($sig.Status -ne "Valid") { throw "$exe is not validly signed: $($sig.Status) $($sig.StatusMessage)" }
    if (-not $sig.TimeStamperCertificate) { throw "$exe has no timestamp" }
    $signer = $sig.SignerCertificate.GetNameInfo([Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false)
    Write-Host "$exe signed by $($sig.SignerCertificate.Subject)"
}

# The same sums and manifest the release workflow writes, now for the
# signed exes.
$sums = Join-Path $work "SHA256SUMS.txt"
$exes | Sort-Object | ForEach-Object {
    "$((Get-FileHash (Join-Path $work $_) -Algorithm SHA256).Hash.ToLower())  $_"
} | Set-Content -Encoding ascii $sums
foreach ($entry in $manifest.files) {
    $path = Join-Path $work $entry.name
    $entry.size = (Get-Item $path).Length
    $entry.sha256 = (Get-FileHash $path -Algorithm SHA256).Hash.ToLower()
    $entry.authenticode = "$((Get-AuthenticodeSignature $path).Status)"
}
$manifest | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8NoBOM (Join-Path $work "BUILD-MANIFEST.json")
Get-Content $sums

Write-Host "Uploading the signed files to the draft"
Invoke-Native gh @("release", "upload", $Tag, "-R", $Repository, "--clobber",
    (Join-Path $work "ferossh.exe"), (Join-Path $work "ferossh-desktop.exe"), $sums, (Join-Path $work "BUILD-MANIFEST.json"))

Write-Host "Attesting the signed files"
$started = (Get-Date).ToUniversalTime()
Invoke-Native gh @("workflow", "run", "attest-release.yml", "-R", $Repository, "-f", "tag=$Tag")
$run = $null
for ($i = 0; $i -lt 30 -and -not $run; $i++) {
    Start-Sleep -Seconds 5
    $run = gh run list -R $Repository --workflow attest-release.yml --event workflow_dispatch --limit 5 --json databaseId,createdAt |
        ConvertFrom-Json | Where-Object { ([datetime]$_.createdAt).ToUniversalTime() -ge $started.AddSeconds(-30) } |
        Select-Object -First 1
}
if (-not $run) { throw "the attest workflow did not start; run it from the Actions tab with tag $Tag, then publish the draft" }
Invoke-Native gh @("run", "watch", "$($run.databaseId)", "-R", $Repository, "--exit-status")

Write-Host "Publishing $Tag"
$notes = Join-Path $work "notes.md"
[IO.File]::WriteAllText($notes, $release.body.Replace("<!-- signer -->", $signer), [Text.UTF8Encoding]::new($false))
Invoke-Native gh @("release", "edit", $Tag, "-R", $Repository, "--notes-file", $notes, "--draft=false")

$winget = Join-Path $work "winget"
& (Join-Path $PSScriptRoot "winget-manifest.ps1") -Version $Tag.TrimStart("v") -Sums $sums -OutDir $winget -Repository $Repository
Write-Host ""
Write-Host "Published: https://github.com/$Repository/releases/tag/$Tag"
Write-Host "winget manifests for the signed ferossh.exe: $winget"
