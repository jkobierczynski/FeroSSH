# Builds the release exes the way the release workflow does, so anyone can
# rebuild a published release and get the same bytes.
#
#   git checkout v1.0.0
#   pwsh scripts/build-release.ps1
#   Get-FileHash dist\*.exe
#
# From 1.0 on, the hashes match the unsigned hashes published with each
# release. Needs rustup (it installs the toolchain pinned in
# rust-toolchain.toml), Visual Studio or its Build Tools with the MSVC
# v14.44 toolset ("MSVC v143 - VS 2022 C++ x64/x86 build tools (v14.44)",
# the default in VS 2022 17.14, an extra component in VS 2026) and Windows
# SDK 10.0.26100.
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
# - a pinned MSVC toolset and Windows SDK ($MsvcToolset and $WindowsSdk
#   below): the linker and the static C runtime (libcmt, libucrt) end up
#   in the exe, and Visual Studio's newest toolset changes with every
#   Visual Studio update, so a build with "whatever is installed" differs
#   from one made a week earlier;
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

# The MSVC toolset and Windows SDK every release is built with. Changing
# either changes the exes' bytes; do it in a commit of its own.
$MsvcToolset = "14.44"
$WindowsSdk = "10.0.26100.0"

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

# Loads the environment of Visual Studio's vcvarsall.bat for the pinned
# toolset and SDK into this PowerShell session (PATH, LIB, INCLUDE, and
# VCINSTALLDIR, which makes rustc and the cc crate use the link.exe and
# cl.exe on the PATH instead of the newest ones Visual Studio has).
function Enter-PinnedMsvc {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path $vswhere)) { throw "Visual Studio (or its Build Tools) is not installed: vswhere.exe not found" }
    $hostArch = if ([Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq "Arm64") { "arm64" } else { "x64" }
    # Native tools on an ARM64 PC when that toolset has them, else the x64
    # ones (emulated there); the same compiler and linker either way.
    $hosts = if ($hostArch -eq "arm64") { @("arm64", "x64") } else { @("x64") }
    $found = $null
    foreach ($vs in @(& $vswhere -all -products * -property installationPath)) {
        foreach ($h in $hosts) {
            $link = Get-ChildItem (Join-Path $vs "VC\Tools\MSVC") -Directory -Filter "$MsvcToolset.*" -ErrorAction SilentlyContinue |
                Sort-Object Name -Descending |
                ForEach-Object { Join-Path $_.FullName "bin\Host$h\$Arch\link.exe" } |
                Where-Object { Test-Path $_ } | Select-Object -First 1
            if ($link) { $found = @{ vs = $vs; host = $h }; break }
        }
        if ($found) { break }
    }
    if (-not $found) {
        throw ("MSVC toolset v$MsvcToolset for $Arch not found. Install it with the Visual Studio Installer: " +
            "Individual components > `"MSVC v143 - VS 2022 C++ $(if ($Arch -eq 'arm64') { 'ARM64/ARM64EC' } else { 'x64/x86' }) build tools (v$MsvcToolset)`".")
    }
    $vcvarsArg = if ($found.host -eq $Arch) { $Arch } else { "$($found.host)_$Arch" }
    $vcvarsall = Join-Path $found.vs "VC\Auxiliary\Build\vcvarsall.bat"
    # A .cmd file, so the quotes around the path reach cmd.exe as they are.
    $cmd = Join-Path ([IO.Path]::GetTempPath()) "ferossh-vcvars-$PID.cmd"
    Set-Content -Path $cmd -Encoding ascii -Value @(
        "@echo off",
        "call `"$vcvarsall`" $vcvarsArg $WindowsSdk -vcvars_ver=$MsvcToolset >nul",
        "if errorlevel 1 exit /b 1",
        "set"
    )
    try {
        $vars = & cmd.exe /d /c $cmd
        if ($LASTEXITCODE) { throw "vcvarsall.bat $vcvarsArg $WindowsSdk -vcvars_ver=$MsvcToolset failed ($LASTEXITCODE)" }
    } finally {
        Remove-Item $cmd -ErrorAction SilentlyContinue
    }
    foreach ($line in $vars) {
        if ($line -match '^([^=]+)=(.*)$') { [Environment]::SetEnvironmentVariable($Matches[1], $Matches[2]) }
    }
    if ($env:VCToolsVersion -notlike "$MsvcToolset.*") {
        throw "vcvarsall.bat set up MSVC $env:VCToolsVersion, not v$MsvcToolset"
    }
    if ($env:WindowsSDKVersion.TrimEnd("\") -ne $WindowsSdk) {
        throw "vcvarsall.bat set up Windows SDK $env:WindowsSDKVersion, not $WindowsSdk (is that SDK installed?)"
    }
}

# What built the exes, printed and written next to them, so two builds that
# differ can be told apart by their tools (CI compares these files when the
# exes differ).
function Get-BuildTools {
    $lines = @()
    $lines += "msvc: $env:VCToolsVersion"
    $lines += "windows-sdk: $($env:WindowsSDKVersion.TrimEnd('\')) (ucrt $($env:UCRTVersion))"
    $lines += "link.exe: $((Get-Item (Get-Command link.exe).Source).VersionInfo.FileVersion)"
    if ($Arch -eq "arm64") {
        $lines += "clang: $((& clang --version | Select-Object -First 1))"
    } else {
        $lines += "cl.exe: $((Get-Item (Get-Command cl.exe).Source).VersionInfo.FileVersion)"
    }
    $lines += & rustc -vV | Where-Object { $_ -match '^(rustc|commit-hash|LLVM version)' } | ForEach-Object { "rustc $_" }
    $lines
}

Push-Location $root
try {
    Enter-PinnedMsvc
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
    $tools = Get-BuildTools
    $tools | Set-Content -Encoding utf8NoBOM (Join-Path $dist "build-tools-$Arch.txt")
    "Build tools:"
    $tools | ForEach-Object { "  $_" }

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
