# Signs a draft release's exes on this PC and publishes the release.
#
#   pwsh scripts/sign-release.ps1 -Tag v1.0.0-rc2
#
# For releases made with the LOCAL_SIGNING repository variable set: the
# release workflow then leaves a draft release with the unsigned exes. This
# script, run from a checkout of the repository:
#
#   1. downloads the unsigned exes and BUILD-MANIFEST.json that the release
#      run for the tag kept as workflow artifacts, and checks with
#      `gh attestation verify` that each was built by release.yml for this
#      tag, at the commit the tag points to, on GitHub's runners; the
#      manifest must name that tag and commit and the attested exes' hashes
#      (scripts/release-provenance.ps1). A file in the draft release is
#      never trusted because another file in the draft vouches for it;
#   2. checks the Linux agents in the draft the same way (they are
#      published as CI built them);
#   3. signs both exes with signtool and the code signing certificate whose
#      subject contains -Subject (a Certum card, or SimplySign Desktop
#      logged in), with an RFC 3161 timestamp from Certum, and checks each
#      signed exe is its attested unsigned exe plus the signature;
#   4. writes SHA256SUMS.txt and BUILD-MANIFEST.json for the signed exes
#      (each exe keeps its attested sha256_unsigned, and "signing" records
#      the verified build: workflow, run, commit and CI manifest hash) and
#      uploads the exes, the sums and the manifest to the draft;
#   5. runs the "Attest a signed release" workflow on them, which checks
#      them against CI's build again before attesting, and waits;
#   6. names the signer in the release notes, publishes the release and
#      writes the winget manifests for the signed ferossh.exe.
#
# It needs PowerShell 7, the GitHub CLI 2.93 or later (gh, logged in with
# write access; `gh attestation verify` with --source-ref and
# --source-digest) and signtool from the Windows SDK. Nothing is published
# until every check passes; run it again after a failure (it starts again
# from CI's unsigned exes, so it signs again).

#Requires -Version 7

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
. (Join-Path $PSScriptRoot "release-provenance.ps1")
$exes = @($ReleaseExeArtifacts.Keys)

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

Assert-GhAttestationSupport
$signtool = Find-SignTool
# The attestations name the repository as GitHub spells it.
$Repository = Get-CanonicalRepository $Repository

$release = gh release view $Tag -R $Repository --json isDraft,body | ConvertFrom-Json
if ($LASTEXITCODE) { throw "release $Tag not found in $Repository" }
if (-not $release.isDraft) { throw "release $Tag is already published; this script only signs drafts" }

$work = Join-Path ([IO.Path]::GetTempPath()) "ferossh-sign-$Tag"
Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $work | Out-Null
$out = Join-Path $work "release"
New-Item -ItemType Directory $out | Out-Null

# Sign only what CI built, as its attestations prove: not whatever the
# draft holds, which anyone with release write access could have replaced
# together with BUILD-MANIFEST.json.
$build = Get-VerifiedReleaseBuild -Repository $Repository -Tag $Tag -OutDir (Join-Path $work "ci")
$manifest = $build.Manifest

Write-Host "Downloading the draft's Linux agents to $out"
Invoke-Native gh @("release", "download", $Tag, "-R", $Repository, "-D", $out, "-p", "ferostream-agent*")
Assert-ReleaseAgents -Dir $out -Build $build -Repository $Repository -Tag $Tag
$agents = @($manifest.files | Where-Object name -like "ferostream-agent*" | ForEach-Object name)

foreach ($exe in $exes) { Copy-Item $build.Exes[$exe] (Join-Path $out $exe) }
Write-Host "Signing with the certificate for '$Subject' (SimplySign or the card may ask for your PIN or a code)"
foreach ($exe in $exes) {
    Invoke-Native $signtool @("sign", "/n", $Subject, "/fd", "sha256", "/tr", $Timestamp, "/td", "sha256",
        "/d", "FeroSSH", "/du", "https://github.com/$Repository", (Join-Path $out $exe))
}
$signer = $null
$signerSubject = $null
foreach ($exe in $exes) {
    $path = Join-Path $out $exe
    $sig = Get-AuthenticodeSignature $path
    if ($sig.Status -ne "Valid") { throw "$exe is not validly signed: $($sig.Status) $($sig.StatusMessage)" }
    if (-not $sig.TimeStamperCertificate) { throw "$exe has no timestamp" }
    # The signed exe must be CI's attested exe with a signature added and
    # nothing else changed.
    if (-not (Test-SignedFromUnsigned -Signed $path -Unsigned $build.Exes[$exe])) {
        throw "signed $exe differs from CI's attested unsigned exe in more than its signature"
    }
    $signer = $sig.SignerCertificate.GetNameInfo([Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false)
    $signerSubject = $sig.SignerCertificate.Subject
    Write-Host "$exe signed by $signerSubject"
}

# The same sums and manifest the release workflow writes, now for the
# signed exes. Each exe keeps sha256_unsigned, the hash its attestation
# from the release workflow covers; "signing" says which build was checked.
$sums = Join-Path $out "SHA256SUMS.txt"
($exes + $agents) | Sort-Object | ForEach-Object {
    "$(Get-Sha256 (Join-Path $out $_))  $_"
} | Set-Content -Encoding ascii $sums
foreach ($entry in $manifest.files | Where-Object name -in $exes) {
    $path = Join-Path $out $entry.name
    $entry.size = (Get-Item $path).Length
    $entry.sha256 = Get-Sha256 $path
    $entry.authenticode = "$((Get-AuthenticodeSignature $path).Status)"
}
$manifest | Add-Member -Force -NotePropertyName signing -NotePropertyValue ([ordered]@{
    signed_by      = $signerSubject
    verified_build = [ordered]@{
        workflow        = "$ReleaseWorkflowPath@refs/tags/$Tag"
        run             = $build.Run
        commit          = $build.Commit
        manifest_sha256 = $build.ManifestHash
    }
})
$manifestPath = Join-Path $out "BUILD-MANIFEST.json"
$manifest | ConvertTo-Json -Depth 5 | Set-Content -Encoding utf8NoBOM $manifestPath
Get-Content $sums

Write-Host "Uploading the signed files to the draft"
Invoke-Native gh @("release", "upload", $Tag, "-R", $Repository, "--clobber",
    (Join-Path $out "ferossh.exe"), (Join-Path $out "ferossh-desktop.exe"), $sums, $manifestPath)

Write-Host "Attesting the signed files"
# A fresh id in the run's name, so this waits for the run it started and
# not for another dispatch of the workflow that happens to overlap.
$request = [guid]::NewGuid().ToString("N")
Invoke-Native gh @("workflow", "run", "attest-release.yml", "-R", $Repository, "-f", "tag=$Tag", "-f", "request=$request")
$run = $null
for ($i = 0; $i -lt 30 -and -not $run; $i++) {
    Start-Sleep -Seconds 5
    $run = gh run list -R $Repository --workflow attest-release.yml --event workflow_dispatch --limit 20 --json databaseId,displayTitle |
        ConvertFrom-Json | Where-Object displayTitle -eq "Attest $Tag ($request)" |
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
