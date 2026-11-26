//! Lists the skills under `skills/` at the repository root into a table of
//! `include_str!` entries, so the pack the binary carries is exactly the
//! directories on disk that hold a `SKILL.md`: its `SKILL.md` and
//! everything under its `references/`.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../../skills");
    let root = root.canonicalize().unwrap_or(root);
    println!("cargo:rerun-if-changed={}", root.display());

    let mut skills: Vec<String> = Vec::new();
    if let Ok(entries) = fs::read_dir(&root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join("SKILL.md").is_file() {
                skills.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    skills.sort();

    let mut out = String::from("&[\n");
    for skill in &skills {
        let dir = root.join(skill);
        let mut files = vec![dir.join("SKILL.md")];
        walk(&dir.join("references"), &mut files);
        files.sort();
        let _ = writeln!(out, "    ({skill:?}, &[");
        for file in files {
            println!("cargo:rerun-if-changed={}", file.display());
            let rel = file
                .strip_prefix(&dir)
                .expect("file under its skill")
                .to_string_lossy()
                .replace('\\', "/");
            let _ = writeln!(
                out,
                "        ({rel:?}, include_str!({:?})),",
                file.display().to_string()
            );
        }
        out.push_str("    ]),\n");
    }
    out.push_str("]\n");

    let dest = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("pack.rs");
    fs::write(dest, out).expect("write pack.rs");
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
