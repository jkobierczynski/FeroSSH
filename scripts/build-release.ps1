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
# `-Arch arm64` builds the Windows on ARM exes instead (ferossh-arm64.exe,
# ferossh-vnc-arm64.exe, ferossh-desktop-arm64.exe), on an ARM64 PC or
# cross-compiled from x64. It also needs MSVC's ARM64 build tools and
# clang (LLVM, or Visual Studio's "C++ Clang Compiler for Windows") on the
# PATH: ring compiles its C code for ARM64 with clang only. The release's
# ARM64 exes are built on an ARM64 runner.
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
    [string]$OutDir = "dist",
    # x64 (the default) or arm64.
    [ValidateSet("x64", "arm64")]
    [string]$Arch = "x64"
)

$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME ".cargo" }
$cargoHome = [IO.Path]::GetFullPath($cargoHome)

$triple = if ($Arch -eq "arm64") { "aarch64-pc-windows-msvc" } else { "x86_64-pc-windows-msvc" }
# TOML literal strings, so the backslashes in Windows paths stay as they are.
$remap = "target.$triple.rustflags=['--remap-path-prefix=$root=/ferossh', '--remap-path-prefix=$cargoHome=/cargo']"
if ($Arch -eq "arm64") {
    # All C code (ring, and zlib in the viewer builds) goes through clang,
    # which ring insists on for ARM64. -mno-incremental-linker-compatible
    # is clang's /Brepro (no timestamp in the objects); -cfguard is
    # Control Flow Guard for the C code too.
    $env:CC_aarch64_pc_windows_msvc = "clang"
    $env:CFLAGS_aarch64_pc_windows_msvc = "-mno-incremental-linker-compatible -Xclang -cfguard"
    $targetArgs = @("--target", $triple)
    $built = Join-Path $root "target\$triple\release\ferossh.exe"
    $suffix = "-arm64"
} else {
    # /guard:cf: Control Flow Guard for the C code too.
    $env:CFLAGS = "/Brepro /guard:cf"
    $targetArgs = @()
    $built = Join-Path $root "target\release\ferossh.exe"
    $suffix = ""
}

Push-Location $root
try {
    if ($Arch -eq "arm64") {
        # The pinned toolchain's standard library for ARM64 (already there
        # on an ARM64 PC).
        & rustup target add $triple
        if ($LASTEXITCODE) { exit $LASTEXITCODE }
    }
    $dist = Join-Path $root $OutDir
    New-Item -ItemType Directory -Force $dist | Out-Null

    # Three exes from one package: the standard one, with none of the
    # viewer's code; ferossh-vnc.exe with the VNC viewer; ferossh-desktop.exe
    # with the VNC and RDP viewers. ARM64 names end in -arm64.
    $variants = [ordered]@{
        "ferossh$suffix.exe"         = @()
        "ferossh-vnc$suffix.exe"     = @("--features", "vnc-viewer")
        "ferossh-desktop$suffix.exe" = @("--features", "desktop-viewer")
    }
    foreach ($name in $variants.Keys) {
        $features = $variants[$name]
        & cargo build --release --locked -p ferossh --config $remap @targetArgs @features
        if ($LASTEXITCODE) { exit $LASTEXITCODE }
        Copy-Item $built (Join-Path $dist $name) -Force
    }

    $variants.Keys | ForEach-Object { Get-Item (Join-Path $dist $_) } | ForEach-Object {
        "$((Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower())  $($_.Name)"
    }
} finally {
    Pop-Location
}
