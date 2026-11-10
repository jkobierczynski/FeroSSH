# Builds the release exes the way the release workflow does, so anyone can
# rebuild a published release and get the same bytes.
#
#   git checkout v1.0.0
#   pwsh scripts/build-release.ps1
#   Get-FileHash dist\*.exe
#
# From 1.0 on, the hashes match the unsigned hashes published with each
# release. Needs rustup (it installs the toolchain pinned in
# rust-toolchain.toml), MSVC Build Tools and the Windows SDK.
#
# What makes the build reproducible:
# - the pinned toolchain and Cargo.lock (--locked);
# - /Brepro for the linker and the C compiler (ring's C code), which
#   replaces the timestamps in the exe with a hash of its contents
#   (.cargo/config.toml and CFLAGS below);
# - --remap-path-prefix, which turns the checkout folder and CARGO_HOME
#   into fixed names in panic messages, so the folder you build in does not
#   matter.

param(
    # Where the exes go, relative to the repository root.
    [string]$OutDir = "dist"
)

$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME ".cargo" }
$cargoHome = [IO.Path]::GetFullPath($cargoHome)

# TOML literal strings, so the backslashes in Windows paths stay as they are.
$remap = "target.x86_64-pc-windows-msvc.rustflags=['--remap-path-prefix=$root=/ferossh', '--remap-path-prefix=$cargoHome=/cargo']"
# /guard:cf: Control Flow Guard for the C code too.
$env:CFLAGS = "/Brepro /guard:cf"

Push-Location $root
try {
    $dist = Join-Path $root $OutDir
    New-Item -ItemType Directory -Force $dist | Out-Null

    # Three exes from one package: the standard one, with none of the
    # viewer's code; ferossh-vnc.exe with the VNC viewer; ferossh-desktop.exe
    # with the VNC and RDP viewers.
    $variants = [ordered]@{
        "ferossh.exe"         = @()
        "ferossh-vnc.exe"     = @("--features", "vnc-viewer")
        "ferossh-desktop.exe" = @("--features", "desktop-viewer")
    }
    foreach ($name in $variants.Keys) {
        $features = $variants[$name]
        & cargo build --release --locked -p ferossh --config $remap @features
        if ($LASTEXITCODE) { exit $LASTEXITCODE }
        Copy-Item (Join-Path $root "target\release\ferossh.exe") (Join-Path $dist $name) -Force
    }

    Get-ChildItem (Join-Path $dist "*.exe") | Sort-Object Name | ForEach-Object {
        "$((Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower())  $($_.Name)"
    }
} finally {
    Pop-Location
}
