//! Lists the example project under `project/` into a table of
//! `include_bytes!` entries, so the project `apic demo` writes is exactly
//! the files on disk, in path order.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let root = manifest.join("project");
    println!("cargo:rerun-if-changed={}", root.display());

    let mut files = Vec::new();
    walk(&root, &mut files);
    let mut entries: Vec<(String, PathBuf)> = files
        .into_iter()
        .map(|file| {
            let rel = file
                .strip_prefix(&root)
                .expect("file under the project")
                .to_string_lossy()
                .replace('\\', "/");
            (rel, file)
        })
        .collect();
    // A directory's files come where the directory sorts among its
    // siblings, which is plain order of the slash-separated paths.
    entries.sort();

    let mut out = String::from("&[\n");
    for (rel, file) in &entries {
        println!("cargo:rerun-if-changed={}", file.display());
        let _ = writeln!(out, "    ({rel:?}, include_bytes!({:?})),", file.display().to_string());
    }
    out.push_str("]\n");

    let dest = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("project.rs");
    fs::write(dest, out).expect("write project.rs");
}

fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, files);
        } else {
            files.push(path);
        }
    }
}
