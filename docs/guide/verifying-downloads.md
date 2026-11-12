# Verifying a download

From 1.0 on, every release can be checked three ways. Each one is stronger than the one before it.

## 1. Authenticode signature

When the release is code-signed, right-click the exe, choose Properties > Digital Signatures, and check the signer. In PowerShell:

```powershell
Get-AuthenticodeSignature .\ferossh.exe
```

## 2. Checksums

Each release has `SHA256SUMS.txt`, with the SHA-256 of each exe as you download it. In PowerShell:

```powershell
(Get-FileHash .\ferossh.exe).Hash.ToLower()
```

The result must equal the line for `ferossh.exe`.

## 3. Build attestation

The release workflow signs a build provenance attestation for the exes, `SHA256SUMS.txt` and `BUILD-MANIFEST.json`, with the workflow's own identity through GitHub's Sigstore service. It proves that the file was built by FeroSSH's release workflow from a given commit, on GitHub's runners, and that nobody changed it afterwards. With the GitHub CLI:

```
gh attestation verify ferossh.exe -R jkobierczynski/FeroSSH
```

When the exes are signed on the maintainer's PC (from v1.0.0-rc2 on), the release workflow attests the unsigned exes it built, and after signing a second workflow (*Attest a signed release*) attests the signed files that are published. The second attestation proves the files came from FeroSSH's repository unchanged since; that they match the build is shown by `BUILD-MANIFEST.json`, below, and by rebuilding.

Neither the signing script nor that second workflow trusts the files in the draft release. Before anything is signed, `scripts/sign-release.ps1` takes the unsigned exes and `BUILD-MANIFEST.json` from the release run itself and checks each with `gh attestation verify`: the attestation must come from `.github/workflows/release.yml` running for that release's tag, at the commit the tag points to, on GitHub's own runners, and the manifest must name that tag, that commit and those exes' hashes. After signing it checks that each signed exe is the attested unsigned exe plus the signature and nothing else. The *Attest a signed release* workflow repeats all of these checks against the files in the draft before it attests them. From the first release after v1.3.0, `BUILD-MANIFEST.json` also has a `signing` section naming the signer and the build that was checked: the workflow, its run, the commit and the SHA-256 of the release workflow's own manifest.

To check the link yourself, verify the unsigned exe's attestation as above with the release workflow's identity, for example:

```
gh attestation verify ferossh.exe -R jkobierczynski/FeroSSH --signer-workflow jkobierczynski/FeroSSH/.github/workflows/release.yml --source-ref refs/tags/<tag>
```

run on an exe you rebuilt with the steps below (it has the unsigned hash), or on the signed download with `--signer-workflow jkobierczynski/FeroSSH/.github/workflows/attest-release.yml --source-ref refs/heads/main` (that workflow only attests when it runs from `main`).

`BUILD-MANIFEST.json` lists the tag, the commit, the Rust toolchain and, for each exe, its size, its SHA-256 as downloaded (`sha256`) and its SHA-256 before code signing (`sha256_unsigned`).

## Rebuilding a release yourself

FeroSSH's builds are reproducible: the same source gives the same bytes, wherever it is built. CI checks this for every change by building twice, on two machines and in two folders, and comparing. To rebuild a release on your own PC you need Git, rustup, Visual Studio Build Tools with the MSVC toolchain, and the Windows SDK. Then:

```powershell
git clone https://github.com/jkobierczynski/FeroSSH
cd FeroSSH
git checkout v1.0.0
pwsh scripts/build-release.ps1
```

For the ARM64 exes, check out the tag of a release that has them, and run `pwsh scripts/build-release.ps1 -Arch arm64` on an ARM64 PC. Compare `ferossh-arm64.exe` and `ferossh-desktop-arm64.exe` with the manifest; the script also builds `ferossh-vnc-arm64.exe`, which releases do not publish. It also needs Visual Studio's ARM64 build tools and clang on the PATH (LLVM, or Visual Studio's "C++ Clang Compiler for Windows"), because ring compiles its C code for ARM64 with clang. CI checks these exes the same way, on ARM64 runners. Cross-compiling from an x64 PC with the same command should work too, but only the native ARM64 build is checked against the release.

The script prints the SHA-256 of each exe it built. Those of the published exes must equal the `sha256_unsigned` values in that release's `BUILD-MANIFEST.json`. (`ferossh-vnc.exe` and `ferossh-vnc-arm64.exe` are built but not published.) They differ from `SHA256SUMS.txt` only when the release is code-signed, because the signature is added to the exe after it is built.

The script builds with the toolchain pinned in `rust-toolchain.toml` (rustup installs it), the dependencies pinned in `Cargo.lock`, `/Brepro` so the exe holds no timestamps, and `--remap-path-prefix` so the folder you build in does not end up in the exe.
