//! Embeds the application manifest (per-monitor DPI v2, Windows 10+) and a
//! version resource (the Details tab of the exe's Properties, which code
//! signing checks against), and makes Windows load the DLLs the exe imports
//! from System32 only, so a DLL planted next to a downloaded ferossh.exe is
//! never picked up.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=ferossh.manifest");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|env| env == "msvc");
    if windows && msvc {
        let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
        let manifest = std::path::Path::new(&dir).join("ferossh.manifest");
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
            manifest.display()
        );
        // LOAD_LIBRARY_SEARCH_SYSTEM32, for the import table (Windows 10
        // 1607 and later; older versions ignore it).
        println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");

        // link.exe takes a compiled .res file as an input of its own, so
        // the resource is written here byte for byte: no rc.exe, and the
        // same bytes on every machine.
        let out = std::env::var("OUT_DIR").unwrap_or_default();
        let res = std::path::Path::new(&out).join("version.res");
        let version = env!("CARGO_PKG_VERSION");
        let number = [
            env!("CARGO_PKG_VERSION_MAJOR"),
            env!("CARGO_PKG_VERSION_MINOR"),
            env!("CARGO_PKG_VERSION_PATCH"),
        ]
        .map(|n| n.parse::<u16>().unwrap_or(0));
        let strings = [
            ("CompanyName", "FeroSSH contributors"),
            ("FileDescription", "FeroSSH"),
            ("FileVersion", version),
            ("InternalName", "ferossh"),
            (
                "LegalCopyright",
                "Copyright (c) FeroSSH contributors. MIT OR Apache-2.0.",
            ),
            ("OriginalFilename", "ferossh.exe"),
            ("ProductName", "FeroSSH"),
            ("ProductVersion", version),
        ];
        let bytes = version_res([number[0], number[1], number[2], 0], &strings);
        if std::fs::write(&res, bytes).is_ok() {
            println!("cargo:rustc-link-arg-bins={}", res.display());
        }
    }
}

/// A .res file holding one RT_VERSION resource, laid out as rc.exe and
/// llvm-rc write it (llvm-rc gives the same bytes for the same input).
fn version_res(version: [u16; 4], strings: &[(&str, &str)]) -> Vec<u8> {
    let ms = (u32::from(version[0]) << 16) | u32::from(version[1]);
    let ls = (u32::from(version[2]) << 16) | u32::from(version[3]);
    // VS_FIXEDFILEINFO: signature, structure version, file and product
    // version, flags mask and flags, VOS_NT_WINDOWS32, VFT_APP, subtype,
    // date.
    let fixed: Vec<u8> = [
        0xFEEF_04BD,
        0x0001_0000,
        ms,
        ls,
        ms,
        ls,
        0x3F,
        0,
        0x0004_0004,
        1,
        0,
        0,
        0,
    ]
    .into_iter()
    .flat_map(u32::to_le_bytes)
    .collect();
    let strings: Vec<Vec<u8>> = strings
        .iter()
        .map(|(key, value)| {
            let value = utf16z(value);
            node(key, &value, (value.len() / 2) as u16, true, &[])
        })
        .collect();
    // US English, Unicode.
    let table = node("040904B0", &[], 0, true, &strings);
    let string_info = node("StringFileInfo", &[], 0, true, &[table]);
    let translation = node("Translation", &[0x09, 0x04, 0xB0, 0x04], 4, false, &[]);
    let var_info = node("VarFileInfo", &[], 0, true, &[translation]);
    let data = node(
        "VS_VERSION_INFO",
        &fixed,
        52,
        false,
        &[string_info, var_info],
    );

    // A .res file starts with an empty entry, then each resource has a
    // header: data size, header size, type RT_VERSION (16), name 1, data
    // version, flags (moveable, pure), language 0x0409, version,
    // characteristics.
    let mut res = vec![
        0, 0, 0, 0, 0x20, 0, 0, 0, 0xFF, 0xFF, 0, 0, 0xFF, 0xFF, 0, 0,
    ];
    res.extend([0; 16]);
    res.extend((data.len() as u32).to_le_bytes());
    res.extend(0x20u32.to_le_bytes());
    res.extend([0xFF, 0xFF, 16, 0, 0xFF, 0xFF, 1, 0]);
    res.extend(0u32.to_le_bytes());
    res.extend([0x30, 0, 0x09, 0x04]);
    res.extend([0; 8]);
    res.extend(data);
    pad4(&mut res);
    res
}

/// One block of a version resource: length, value length, type (1 for
/// text), key, then its value and child blocks, each 32-bit aligned.
fn node(key: &str, value: &[u8], value_len: u16, text: bool, children: &[Vec<u8>]) -> Vec<u8> {
    // The length goes first; it is filled in once the block is complete.
    let mut v = vec![0, 0];
    v.extend(value_len.to_le_bytes());
    v.extend(u16::from(text).to_le_bytes());
    v.extend(utf16z(key));
    pad4(&mut v);
    v.extend(value);
    for child in children {
        pad4(&mut v);
        v.extend(child);
    }
    let len = (v.len() as u16).to_le_bytes();
    v.splice(..2, len);
    v
}

fn utf16z(s: &str) -> Vec<u8> {
    s.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn pad4(v: &mut Vec<u8>) {
    while !v.len().is_multiple_of(4) {
        v.push(0);
    }
}
