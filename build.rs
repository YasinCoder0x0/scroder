use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=resources/scroder.rc");
    println!("cargo:rerun-if-changed=resources/Scroder.ico");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let Some(rc) = find_rc() else {
        println!("cargo:warning=rc.exe was not found; compiling without an application icon");
        return;
    };

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by cargo"));
    let res_file = out_dir.join("scroder.res");

    let status = Command::new(&rc)
        .current_dir(env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string()))
        .arg("/nologo")
        .arg("/fo")
        .arg(&res_file)
        .arg("resources\\scroder.rc")
        .status();

    match status {
        Ok(status) if status.success() && res_file.exists() => {
            println!("cargo:rustc-link-arg={}", res_file.display());
        }
        _ => {
            println!("cargo:warning=compiling the icon resource failed; continuing without it");
        }
    }
}

fn find_rc() -> Option<PathBuf> {
    if let Ok(bin) = env::var("WindowsSdkVerBinPath") {
        let candidate = Path::new(&bin).join("rc.exe");
        if candidate.exists() {
            return Some(candidate);
        }
    }

    if let Ok(sdk) = env::var("WindowsSdkDir") {
        let bin = Path::new(&sdk).join("bin");
        if let Ok(entries) = std::fs::read_dir(&bin) {
            let mut versions: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_dir())
                .collect();
            versions.sort();
            for version in versions.into_iter().rev() {
                let candidate = version.join("x64").join("rc.exe");
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
    }

    // Last resort: rely on PATH (works when started from a Developer prompt).
    let probe = Command::new("rc.exe").arg("/?").output();
    if probe.is_ok() {
        return Some(PathBuf::from("rc.exe"));
    }

    let standard = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    if let Ok(entries) = std::fs::read_dir(standard) {
        let mut versions: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        versions.sort();
        for version in versions.into_iter().rev() {
            let candidate = version.join("x64").join("rc.exe");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    None
}
