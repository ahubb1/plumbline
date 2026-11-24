//! `check-dll`: asserts that a PE file depends only on DLLs that ship with Windows.
//!
//! The dependency list comes from our own PE reader. When `dumpbin.exe` from the MSVC Build
//! Tools can be found, `dumpbin /dependents` is run as well and the two lists must agree.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::pe;

/// DLLs present on every supported Windows install (lower case).
const SYSTEM_DLLS: &[&str] = &[
    "advapi32.dll",
    "bcrypt.dll",
    "bcryptprimitives.dll",
    "cfgmgr32.dll",
    "combase.dll",
    "comctl32.dll",
    "comdlg32.dll",
    "crypt32.dll",
    "cryptbase.dll",
    "d3d11.dll",
    "d3d12.dll",
    "d3dcompiler_47.dll",
    "dbghelp.dll",
    "dcomp.dll",
    "dwmapi.dll",
    "dwrite.dll",
    "dxgi.dll",
    "gdi32.dll",
    "gdi32full.dll",
    "imm32.dll",
    "iphlpapi.dll",
    "kernel32.dll",
    "kernelbase.dll",
    "mpr.dll",
    "msimg32.dll",
    "msvcrt.dll",
    "ncrypt.dll",
    "netapi32.dll",
    "ntdll.dll",
    "ole32.dll",
    "oleacc.dll",
    "oleaut32.dll",
    "opengl32.dll",
    "powrprof.dll",
    "propsys.dll",
    "psapi.dll",
    "rpcrt4.dll",
    "secur32.dll",
    "setupapi.dll",
    "shcore.dll",
    "shell32.dll",
    "shlwapi.dll",
    "ucrtbase.dll",
    "uiautomationcore.dll",
    "user32.dll",
    "userenv.dll",
    "usp10.dll",
    "uxtheme.dll",
    "version.dll",
    "winhttp.dll",
    "wininet.dll",
    "winmm.dll",
    "winspool.drv",
    "wintrust.dll",
    "ws2_32.dll",
    "wtsapi32.dll",
];

/// Name prefixes of the Visual C++ redistributable, which is not part of Windows (lower case).
const REDIST_PREFIXES: &[&str] = &[
    "vcruntime", "msvcp", "concrt", "vccorlib", "vcomp", "vcamp", "mfc", "atl1", "msvcr",
];

/// How a dependency is classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DllClass {
    /// Ships with Windows.
    System,
    /// Part of the Visual C++ redistributable: the target PC may not have it.
    VcRedist,
    /// Not on the list of Windows DLLs.
    Unknown,
}

pub fn classify(name: &str) -> DllClass {
    let lower = name.trim().to_ascii_lowercase();
    // `msvcrt.dll` is the old system CRT and ships with Windows; `msvcr120.dll` and friends do not.
    if lower != "msvcrt.dll" && REDIST_PREFIXES.iter().any(|p| lower.starts_with(p)) {
        return DllClass::VcRedist;
    }
    let api_set = (lower.starts_with("api-ms-win-") || lower.starts_with("ext-ms-win-"))
        && lower.ends_with(".dll");
    if api_set || SYSTEM_DLLS.contains(&lower.as_str()) {
        return DllClass::System;
    }
    DllClass::Unknown
}

/// What the check allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Every dependency must be a known Windows DLL (used for `pdfium.dll`).
    SystemOnly,
    /// The Visual C++ redistributable and the dynamic Universal CRT are refused, other DLLs
    /// are not (used for our own static-CRT exes, whose GUI stack pulls in more Windows DLLs
    /// than the list above tries to name). A static-CRT exe imports neither, so this also
    /// proves `+crt-static` was in effect.
    NoVcRedist,
}

/// True for the DLLs of the dynamic Universal CRT: `ucrtbase.dll` and the `api-ms-win-crt-*`
/// API sets that forward to it. They ship with Windows, but a static-CRT build never imports
/// them.
pub fn is_dynamic_crt(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    lower == "ucrtbase.dll" || lower.starts_with("api-ms-win-crt-")
}

/// Names from `names` that the policy refuses, with the reason for each.
pub fn violations(names: &[String], policy: Policy) -> Vec<String> {
    names
        .iter()
        .filter_map(|name| match (classify(name), policy) {
            (_, Policy::NoVcRedist) if is_dynamic_crt(name) => Some(format!(
                "{name}: dynamic Universal CRT, so the exe was not built with +crt-static"
            )),
            (DllClass::System, _) | (DllClass::Unknown, Policy::NoVcRedist) => None,
            (DllClass::VcRedist, _) => Some(format!(
                "{name}: Visual C++ redistributable, not part of Windows"
            )),
            (DllClass::Unknown, Policy::SystemOnly) => {
                Some(format!("{name}: not a known Windows system DLL"))
            }
        })
        .collect()
}

/// Pulls the DLL names out of `dumpbin /dependents` output.
///
/// The names sit one per line between "Image has the following dependencies:" (and, when
/// present, "Image has the following delay load dependencies:") and the "Summary" block.
pub fn parse_dumpbin_dependents(output: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_list = false;
    for line in output.lines() {
        let line = line.trim();
        if line.starts_with("Image has the following") {
            in_list = true;
        } else if line.starts_with("Summary") {
            in_list = false;
        } else if in_list && !line.is_empty() && !line.contains(char::is_whitespace) {
            names.push(line.to_string());
        }
    }
    names
}

/// Finds `dumpbin.exe` (x64 host and target) through `vswhere.exe`. `None` when the MSVC tools
/// or vswhere are not installed.
pub fn find_dumpbin() -> Option<PathBuf> {
    let vswhere = ["ProgramFiles(x86)", "ProgramFiles"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|dir| {
            Path::new(&dir)
                .join("Microsoft Visual Studio")
                .join("Installer")
                .join("vswhere.exe")
        })
        .find(|p| p.is_file())?;
    let out = Command::new(vswhere)
        .args(["-latest", "-products", "*", "-find"])
        .arg(r"VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe")
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| PathBuf::from(line.trim()))
        .rfind(|p| p.is_file())
}

fn dumpbin_dependents(dumpbin: &Path, file: &Path) -> Result<Vec<String>> {
    let out = Command::new(dumpbin)
        .args(["/nologo", "/dependents"])
        .arg(file)
        .output()
        .with_context(|| format!("could not start {}", dumpbin.display()))?;
    if !out.status.success() {
        bail!("dumpbin /dependents failed with {}", out.status);
    }
    Ok(parse_dumpbin_dependents(&String::from_utf8_lossy(
        &out.stdout,
    )))
}

fn normalised(names: &[String]) -> Vec<String> {
    let mut v: Vec<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
    v.sort();
    v.dedup();
    v
}

/// Result of checking one file.
#[derive(Debug)]
pub struct Report {
    pub imports: Vec<String>,
    /// True when `dumpbin /dependents` also ran and agreed with our reader.
    pub dumpbin_confirmed: bool,
}

/// Checks one PE file against a policy. `use_dumpbin` allows the dumpbin cross-check.
pub fn check_file(file: &Path, policy: Policy, use_dumpbin: bool) -> Result<Report> {
    let data = std::fs::read(file).with_context(|| format!("cannot read {}", file.display()))?;
    let imports = pe::imports(&data)
        .with_context(|| format!("cannot parse {} as a PE image", file.display()))?
        .all();

    let mut dumpbin_confirmed = false;
    if use_dumpbin {
        if let Some(dumpbin) = find_dumpbin() {
            let theirs = dumpbin_dependents(&dumpbin, file)?;
            if normalised(&theirs) != normalised(&imports) {
                bail!(
                    "dependency lists disagree for {}\n  dumpbin:   {theirs:?}\n  PE reader: {imports:?}",
                    file.display()
                );
            }
            dumpbin_confirmed = true;
        }
    }

    let bad = violations(&imports, policy);
    if !bad.is_empty() {
        bail!(
            "{} has {} disallowed import(s):\n  {}",
            file.display(),
            bad.len(),
            bad.join("\n  ")
        );
    }
    Ok(Report {
        imports,
        dumpbin_confirmed,
    })
}

/// The `check-dll` subcommand.
pub fn run(file: &Path, policy: Policy, use_dumpbin: bool) -> Result<()> {
    let report = check_file(file, policy, use_dumpbin)?;
    println!("{}", file.display());
    for name in &report.imports {
        println!("  {name}");
    }
    let source = if report.dumpbin_confirmed {
        "PE import table, confirmed by dumpbin /dependents"
    } else {
        "PE import table (dumpbin not used)"
    };
    let rule = match policy {
        Policy::SystemOnly => "all Windows system DLLs",
        Policy::NoVcRedist => "no Visual C++ redistributable or dynamic CRT DLLs",
    };
    println!("OK: {} import(s), {rule}. Source: {source}.", report.imports.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pe::synth::{build, Spec};
    use crate::util::testdir::TestDir;

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn classifies_system_redist_and_unknown() {
        for name in [
            "KERNEL32.dll",
            "kernel32.DLL",
            "WINSPOOL.DRV",
            "api-ms-win-crt-runtime-l1-1-0.dll",
            "ext-ms-win-ntuser-window-l1-1-0.dll",
            "msvcrt.dll",
            "ucrtbase.dll",
        ] {
            assert_eq!(classify(name), DllClass::System, "{name}");
        }
        for name in [
            "VCRUNTIME140.dll",
            "VCRUNTIME140_1.dll",
            "MSVCP140.dll",
            "msvcr120.dll",
            "CONCRT140.dll",
            "vcomp140.dll",
        ] {
            assert_eq!(classify(name), DllClass::VcRedist, "{name}");
        }
        for name in ["zlib1.dll", "libcrypto-3-x64.dll", "api-ms-win-fake", "pdfium.dll"] {
            assert_eq!(classify(name), DllClass::Unknown, "{name}");
        }
    }

    #[test]
    fn system_only_policy_refuses_redist_and_unknown() {
        let names = strings(&["KERNEL32.dll", "VCRUNTIME140.dll", "zlib1.dll"]);
        let bad = violations(&names, Policy::SystemOnly);
        assert_eq!(bad.len(), 2);
        assert!(bad[0].starts_with("VCRUNTIME140.dll"));
        assert!(bad[1].starts_with("zlib1.dll"));
        assert!(violations(&strings(&["KERNEL32.dll", "GDI32.dll"]), Policy::SystemOnly).is_empty());
    }

    #[test]
    fn no_redist_policy_refuses_only_redist() {
        let names = strings(&["KERNEL32.dll", "MSVCP140.dll", "pdfium.dll"]);
        let bad = violations(&names, Policy::NoVcRedist);
        assert_eq!(bad.len(), 1);
        assert!(bad[0].starts_with("MSVCP140.dll"));
    }

    #[test]
    fn no_redist_policy_refuses_the_dynamic_ucrt() {
        let names = strings(&[
            "ucrtbase.dll",
            "api-ms-win-crt-heap-l1-1-0.dll",
            "api-ms-win-core-synch-l1-2-0.dll",
        ]);
        let bad = violations(&names, Policy::NoVcRedist);
        assert_eq!(bad.len(), 2, "{bad:?}");
        assert!(bad.iter().all(|b| b.contains("crt-static")), "{bad:?}");
        // pdfium.dll's rule still takes the UCRT as part of Windows.
        assert!(violations(&names, Policy::SystemOnly).is_empty());
    }

    #[test]
    fn parses_dumpbin_output() {
        let text = "\r\nDump of file pdfium.dll\r\n\r\nFile Type: DLL\r\n\r\n  Image has the following dependencies:\r\n\r\n    KERNEL32.dll\r\n    api-ms-win-crt-heap-l1-1-0.dll\r\n\r\n  Image has the following delay load dependencies:\r\n\r\n    dbghelp.dll\r\n\r\n  Summary\r\n\r\n        1000 .data\r\n        2000 .rdata\r\n";
        assert_eq!(
            parse_dumpbin_dependents(text),
            ["KERNEL32.dll", "api-ms-win-crt-heap-l1-1-0.dll", "dbghelp.dll"]
        );
        assert!(parse_dumpbin_dependents("Summary\n  1000 .data\n").is_empty());
    }

    #[test]
    fn check_file_passes_system_only_image_and_fails_others() {
        let dir = TestDir::new("checkdll");
        let good = dir.path().join("good.dll");
        std::fs::write(
            &good,
            build(&Spec {
                pe32_plus: true,
                regular: &["KERNEL32.dll", "USER32.dll"],
                delayed: &[],
                delay_rva: true,
            }),
        )
        .unwrap();
        let report = check_file(&good, Policy::SystemOnly, false).unwrap();
        assert_eq!(report.imports, ["KERNEL32.dll", "USER32.dll"]);
        assert!(!report.dumpbin_confirmed);

        // A redistributable import hidden in the delay-load table must still be caught.
        let bad = dir.path().join("bad.dll");
        std::fs::write(
            &bad,
            build(&Spec {
                pe32_plus: true,
                regular: &["KERNEL32.dll"],
                delayed: &["VCRUNTIME140.dll"],
                delay_rva: true,
            }),
        )
        .unwrap();
        let err = check_file(&bad, Policy::SystemOnly, false).unwrap_err().to_string();
        assert!(err.contains("VCRUNTIME140.dll"), "{err}");
        assert!(check_file(&bad, Policy::NoVcRedist, false).is_err());

        let third_party = dir.path().join("third.dll");
        std::fs::write(
            &third_party,
            build(&Spec {
                pe32_plus: true,
                regular: &["KERNEL32.dll", "zlib1.dll"],
                delayed: &[],
                delay_rva: true,
            }),
        )
        .unwrap();
        assert!(check_file(&third_party, Policy::SystemOnly, false).is_err());
        assert!(check_file(&third_party, Policy::NoVcRedist, false).is_ok());

        assert!(check_file(&dir.path().join("absent.dll"), Policy::SystemOnly, false).is_err());
    }

    /// The real acceptance check (plan M0): `pdfium.dll` imports only system DLLs.
    ///
    /// A missing DLL fails the test. It must not pass: a criterion that was not checked is
    /// not met, and every other PDFium-backed test in the workspace needs the DLL anyway.
    #[test]
    fn vendored_pdfium_imports_only_system_dlls() {
        let dll = crate::fetch::Layout::in_workspace().dll();
        assert!(
            dll.is_file(),
            "{} is absent, so its imports cannot be checked: run `cargo run -p xtask -- fetch-pdfium` first",
            dll.display()
        );
        let report = check_file(&dll, Policy::SystemOnly, true).unwrap();
        assert!(!report.imports.is_empty());
    }
}
