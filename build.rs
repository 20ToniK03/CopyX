//! Встраивает иконку приложения (`app.ico`) в EXE через ресурсный файл.
//!
//! `rc.exe` берётся из Windows SDK; если его нет, сборка продолжается без
//! иконки, но с предупреждением.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=app.ico");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let res = out_dir.join("app.res");

    let Some(rc) = find_rc() else {
        println!("cargo:warning=rc.exe не найден: иконка приложения не будет встроена");
        return;
    };

    let status = Command::new(&rc)
        .args(["/nologo", "/fo"])
        .arg(&res)
        .arg("app.rc")
        .status();

    match status {
        Ok(status) if status.success() => {
            println!("cargo:rustc-link-arg={}", res.display());
        }
        _ => println!("cargo:warning=не удалось скомпилировать app.rc (иконка пропущена)"),
    }
}

fn find_rc() -> Option<PathBuf> {
    if let Some(path) = env::var_os("RC") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    for dir in env::split_paths(&env::var_os("PATH").unwrap_or_default()) {
        let candidate = dir.join("rc.exe");
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    let mut kits: Vec<PathBuf> = Vec::new();
    for base in [
        r"C:\Program Files (x86)\Windows Kits",
        r"C:\Program Files\Windows Kits",
    ] {
        let bin = Path::new(base).join("10").join("bin");
        let Ok(entries) = std::fs::read_dir(&bin) else {
            continue;
        };
        for entry in entries.flatten() {
            kits.push(entry.path());
        }
    }
    // Свежие версии SDK — первыми.
    kits.sort_by(|a, b| b.cmp(a));

    kits.into_iter()
        .map(|dir| dir.join("x64").join("rc.exe"))
        .find(|candidate| candidate.is_file())
}
