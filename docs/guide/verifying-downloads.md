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

`BUILD-MANIFEST.json` lists the tag, the commit, the Rust toolchain and, for each exe, its size, its SHA-256 as downloaded (`sha256`) and its SHA-256 before code signing (`sha256_unsigned`).

## Rebuilding a release yourself

FeroSSH's builds are reproducible: the same source gives the same bytes, wherever it is built. CI checks this for every change by building twice, on two machines and in two folders, and comparing. To rebuild a release on your own PC you need Git, rustup, Visual Studio Build Tools with the MSVC toolchain, and the Windows SDK. Then:

```powershell
git clone https://github.com/jkobierczynski/FeroSSH
cd FeroSSH
git checkout v1.0.0
pwsh scripts/build-release.ps1
```

The script prints the SHA-256 of each exe it built. They must equal the `sha256_unsigned` values in that release's `BUILD-MANIFEST.json`. They differ from `SHA256SUMS.txt` only when the release is code-signed, because the signature is added to the exe after it is built.

The script builds with the toolchain pinned in `rust-toolchain.toml` (rustup installs it), the dependencies pinned in `Cargo.lock`, `/Brepro` so the exe holds no timestamps, and `--remap-path-prefix` so the folder you build in does not end up in the exe.
