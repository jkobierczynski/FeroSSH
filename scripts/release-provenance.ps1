# Checks that a release's files are the ones FeroSSH's release workflow
# built from the release tag, before they are code signed or attested again.
# Dot-sourced by scripts/sign-release.ps1 (on the maintainer's PC) and by
# .github/workflows/attest-release.yml (on a GitHub runner).
#
# Nothing here trusts a file because it sits in the draft release next to
# another file: anyone who can change release assets could replace an exe
# and BUILD-MANIFEST.json together. Instead every file CI built is checked
# with `gh attestation verify` against the build provenance attestation the
# release workflow signed for it, which only that workflow, running for that
# tag on GitHub's runners, can make:
#
#   - signer:  https://github.com/<repo>/.github/workflows/release.yml@refs/tags/<tag>
#   - source:  refs/tags/<tag> at the commit the tag points to now
#   - runner:  GitHub-hosted (self-hosted runners are refused)
#
# BUILD-MANIFEST.json is checked the same way (the release workflow attests
# it too) and must name the same tag and commit and the attested exes'
# hashes. A signed exe must be its attested unsigned exe plus a signature:
# Test-SignedFromUnsigned compares them byte for byte, leaving out only the
# PE checksum, the certificate table's directory entry and the certificate
# table itself, which signing changes.
#
# Needs the GitHub CLI 2.56 or later (for --source-ref and --source-digest),
# logged in with access to the repository.

$ReleaseWorkflowPath = ".github/workflows/release.yml"
# The exes CI builds and the workflow artifacts the release run keeps them in.
$ReleaseExeArtifacts = [ordered]@{
    "ferossh.exe"         = "ferossh-exe"
    "ferossh-desktop.exe" = "ferossh-desktop-viewer-exe"
}
$ReleaseManifestArtifact = "build-manifest"

function Invoke-Gh {
    param([string[]]$Arguments)
    $out = & gh @Arguments
    if ($LASTEXITCODE) { throw "gh $($Arguments[0..1] -join ' ') failed with exit code $LASTEXITCODE" }
    $out
}

function Assert-GhAttestationSupport {
    if (-not (Get-Command gh -ErrorAction SilentlyContinue)) { throw "the GitHub CLI (gh) is not installed" }
    $help = (& gh attestation verify --help 2>&1) -join "`n"
    foreach ($flag in "--cert-identity", "--source-ref", "--source-digest", "--deny-self-hosted-runners") {
        if ($help -notmatch [regex]::Escape($flag)) {
            throw "gh attestation verify has no $flag option: update the GitHub CLI to 2.56 or later (winget upgrade GitHub.cli)"
        }
    }
}

# owner/name exactly as GitHub spells it: the attestation's certificate
# names the workflow with that spelling, and --cert-identity matches it
# exactly.
function Get-CanonicalRepository {
    param([Parameter(Mandatory)] [string]$Repository)
    (Invoke-Gh @("api", "repos/$Repository", "--jq", ".full_name")).Trim()
}

# The commit the tag points to, through an annotated tag object if need be.
function Get-TagCommit {
    param([Parameter(Mandatory)] [string]$Repository, [Parameter(Mandatory)] [string]$Tag)
    $sha = (Invoke-Gh @("api", "repos/$Repository/commits/refs/tags/$Tag", "--jq", ".sha")).Trim()
    if ($sha -notmatch '^[0-9a-f]{40}$') { throw "could not resolve tag $Tag in $Repository to a commit" }
    $sha
}

function Get-Sha256 {
    param([Parameter(Mandatory)] [string]$Path)
    (Get-FileHash $Path -Algorithm SHA256).Hash.ToLower()
}

# Fails unless $Path has a build provenance attestation signed by the
# release workflow running for $Tag at $Commit on a GitHub-hosted runner.
function Assert-ReleaseAttestation {
    param(
        [Parameter(Mandatory)] [string]$Path,
        [Parameter(Mandatory)] [string]$Repository,
        [Parameter(Mandatory)] [string]$Tag,
        [Parameter(Mandatory)] [string]$Commit,
        [string]$Server = "https://github.com"
    )
    $identity = "$Server/$Repository/$ReleaseWorkflowPath@refs/tags/$Tag"
    $out = & gh attestation verify $Path -R $Repository `
        --cert-identity $identity `
        --cert-oidc-issuer "https://token.actions.githubusercontent.com" `
        --source-ref "refs/tags/$Tag" `
        --source-digest $Commit `
        --deny-self-hosted-runners `
        --format json 2>&1
    if ($LASTEXITCODE) {
        throw "$(Split-Path -Leaf $Path) (SHA-256 $(Get-Sha256 $Path)) has no attestation from $identity for commit $Commit; not trusting it.`n$($out -join "`n")"
    }
    Write-Host "  $(Split-Path -Leaf $Path): built by $ReleaseWorkflowPath for $Tag at $($Commit.Substring(0, 12))"
}

# SHA-256 of a PE file without the parts code signing changes: the
# optional header's CheckSum, the certificate table's data directory entry,
# and the certificate table, which must be the end of the file. An unsigned
# exe and the same exe after signtool give the same value.
function Get-PeDigestWithoutSignature {
    param([Parameter(Mandatory)] [string]$Path)
    $bytes = [IO.File]::ReadAllBytes((Convert-Path -LiteralPath $Path))
    $name = Split-Path -Leaf $Path
    if ($bytes.Length -lt 0x40 -or $bytes[0] -ne 0x4D -or $bytes[1] -ne 0x5A) { throw "$name is not a PE file" }
    $pe = [BitConverter]::ToInt32($bytes, 0x3C)
    if ($pe -lt 0x40 -or $pe + 24 -gt $bytes.Length -or [BitConverter]::ToUInt32($bytes, $pe) -ne 0x00004550) { throw "$name has no PE header" }
    $optional = $pe + 24
    $optionalSize = [BitConverter]::ToUInt16($bytes, $pe + 20)
    $magic = [BitConverter]::ToUInt16($bytes, $optional)
    $directories = switch ($magic) { 0x10b { 96 } 0x20b { 112 } default { throw "$name has an unknown optional header ($magic)" } }
    $count = [BitConverter]::ToUInt32($bytes, $optional + $directories - 4)
    if ($count -lt 5 -or $directories + 5 * 8 -gt $optionalSize) { throw "$name has no certificate table entry" }
    $checksum = $optional + 64
    $security = $optional + $directories + 4 * 8
    $tableOffset = [BitConverter]::ToUInt32($bytes, $security)
    $tableSize = [BitConverter]::ToUInt32($bytes, $security + 4)
    $end = $bytes.Length
    if ($tableSize -ne 0) {
        if ([uint64]$tableOffset + $tableSize -ne [uint64]$bytes.Length -or $tableOffset -lt $security + 8) {
            throw "$name has data after or around its certificate table"
        }
        $end = [int]$tableOffset
    }
    $hash = [Security.Cryptography.IncrementalHash]::CreateHash([Security.Cryptography.HashAlgorithmName]::SHA256)
    try {
        $hash.AppendData($bytes, 0, $checksum)
        $hash.AppendData($bytes, $checksum + 4, $security - ($checksum + 4))
        $hash.AppendData($bytes, $security + 8, $end - ($security + 8))
        [pscustomobject]@{
            Digest = [Convert]::ToHexString($hash.GetHashAndReset()).ToLower()
            Signed = $tableSize -ne 0
        }
    } finally { $hash.Dispose() }
}

# True when $Signed is $Unsigned plus a code signature and nothing else.
function Test-SignedFromUnsigned {
    param([Parameter(Mandatory)] [string]$Signed, [Parameter(Mandatory)] [string]$Unsigned)
    $u = Get-PeDigestWithoutSignature $Unsigned
    $s = Get-PeDigestWithoutSignature $Signed
    (-not $u.Signed) -and $s.Signed -and $u.Digest -eq $s.Digest
}

# Downloads the unsigned exes and BUILD-MANIFEST.json that the release run
# for $Tag kept as workflow artifacts into $OutDir and checks them: each
# must carry the release workflow's attestation for the tag's commit, and
# the manifest must name that tag and commit and the exes' hashes. Returns
# the checked tag commit, manifest, manifest hash and exe paths.
#
# The artifacts, not the draft release, are the source: the draft's exes are
# replaced once signed ones are uploaded, and its assets are what an
# attacker with release write access would change. The attestation check is
# what makes them trusted, wherever they came from.
function Get-VerifiedReleaseBuild {
    param(
        [Parameter(Mandatory)] [string]$Repository,
        [Parameter(Mandatory)] [string]$Tag,
        [Parameter(Mandatory)] [string]$OutDir,
        [string]$Server = "https://github.com"
    )
    $commit = Get-TagCommit -Repository $Repository -Tag $Tag
    Write-Host "Tag $Tag is commit $commit"

    # Release runs for this tag: pushed tags run on headBranch = the tag.
    $runs = Invoke-Gh @("run", "list", "-R", $Repository, "--workflow", "release.yml", "--event", "push",
        "--commit", $commit, "--limit", "20", "--json", "databaseId,headBranch,headSha,url") | ConvertFrom-Json
    $run = $runs | Where-Object { $_.headBranch -eq $Tag -and $_.headSha -eq $commit } | Select-Object -First 1
    if (-not $run) { throw "no release workflow run found for $Tag at $commit" }
    Write-Host "Taking CI's unsigned exes from the release run $($run.url)"

    Remove-Item $OutDir -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory $OutDir | Out-Null
    foreach ($artifact in @($ReleaseExeArtifacts.Values) + $ReleaseManifestArtifact) {
        & gh run download "$($run.databaseId)" -R $Repository -n $artifact -D $OutDir
        if ($LASTEXITCODE) {
            throw "could not download the $artifact artifact of $($run.url) (artifacts expire after the repository's retention period; then delete the draft and run the release again)"
        }
    }

    Write-Host "Checking build provenance attestations"
    $manifestPath = Join-Path $OutDir "BUILD-MANIFEST.json"
    Assert-ReleaseAttestation -Path $manifestPath -Repository $Repository -Tag $Tag -Commit $commit -Server $Server
    $exes = [ordered]@{}
    foreach ($exe in $ReleaseExeArtifacts.Keys) {
        $path = Join-Path $OutDir $exe
        Assert-ReleaseAttestation -Path $path -Repository $Repository -Tag $Tag -Commit $commit -Server $Server
        if ((Get-PeDigestWithoutSignature $path).Signed) { throw "$exe from the release run is already signed" }
        $exes[$exe] = $path
    }

    $manifest = Get-Content -Raw $manifestPath | ConvertFrom-Json
    if ($manifest.tag -ne $Tag) { throw "BUILD-MANIFEST.json is for $($manifest.tag), not $Tag" }
    if ($manifest.commit -ne $commit) { throw "BUILD-MANIFEST.json names commit $($manifest.commit), but $Tag is $commit" }
    foreach ($exe in $exes.Keys) {
        $entry = @($manifest.files | Where-Object name -eq $exe)
        $hash = Get-Sha256 $exes[$exe]
        if ($entry.Count -ne 1 -or $entry[0].sha256_unsigned -ne $hash -or $entry[0].sha256 -ne $hash) {
            throw "BUILD-MANIFEST.json does not give the attested SHA-256 $hash for $exe"
        }
    }
    [pscustomobject]@{
        Commit       = $commit
        Run          = $run.url
        Manifest     = $manifest
        ManifestHash = Get-Sha256 $manifestPath
        Exes         = $exes
    }
}

# The Linux agents are published as CI built them (never signed): each one
# in $Dir must carry the release workflow's attestation and match the
# manifest.
function Assert-ReleaseAgents {
    param(
        [Parameter(Mandatory)] [string]$Dir,
        [Parameter(Mandatory)] $Build,
        [Parameter(Mandatory)] [string]$Repository,
        [Parameter(Mandatory)] [string]$Tag,
        [string]$Server = "https://github.com"
    )
    foreach ($entry in $Build.Manifest.files | Where-Object name -like "ferostream-agent*") {
        $path = Join-Path $Dir $entry.name
        if (-not (Test-Path $path)) { throw "the draft has no $($entry.name), which the release workflow built" }
        Assert-ReleaseAttestation -Path $path -Repository $Repository -Tag $Tag -Commit $Build.Commit -Server $Server
        if ((Get-Sha256 $path) -ne $entry.sha256) { throw "$($entry.name) does not match BUILD-MANIFEST.json" }
    }
}
