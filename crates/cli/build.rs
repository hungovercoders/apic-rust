//! Records what the binary was built from, for `apic version`: the
//! compiler's version and, when the tree is a git checkout, the commit,
//! its time and whether the tree had changes.

use std::process::Command;

fn output(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).env("TZ", "UTC").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    Some(text.trim().to_string())
}

fn main() {
    println!("cargo:rerun-if-env-changed=APIC_VERSION");
    println!("cargo:rerun-if-env-changed=APIC_COMMIT");
    println!("cargo:rerun-if-env-changed=APIC_DATE");
    println!("cargo:rerun-if-changed=build.rs");
    for path in ["../../.git/HEAD", "../../.git/index"] {
        if std::path::Path::new(path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let version = output(&rustc, &["--version"])
        .and_then(|v| v.split_whitespace().nth(1).map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=APIC_BUILD_RUSTC=rustc {version}");

    let commit = std::env::var("APIC_COMMIT")
        .ok()
        .filter(|c| !c.is_empty())
        .or_else(|| output("git", &["rev-parse", "HEAD"]))
        .unwrap_or_default();
    println!("cargo:rustc-env=APIC_BUILD_COMMIT={commit}");

    let date = std::env::var("APIC_DATE")
        .ok()
        .filter(|d| !d.is_empty())
        .or_else(|| {
            output(
                "git",
                &["-c", "log.showSignature=false", "show", "-s", "--format=%cd", "--date=format-local:%Y-%m-%dT%H:%M:%SZ", "HEAD"],
            )
        })
        .unwrap_or_default();
    println!("cargo:rustc-env=APIC_BUILD_DATE={date}");

    let dirty = std::env::var("APIC_COMMIT").map_or(true, |c| c.is_empty())
        && output("git", &["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    println!("cargo:rustc-env=APIC_BUILD_DIRTY={dirty}");
}
