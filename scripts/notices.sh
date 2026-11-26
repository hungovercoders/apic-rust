#!/bin/sh
# Generate THIRD_PARTY_NOTICES.md: the licence of every crate compiled into apic.
#
# The crate set is the union across every released platform, not just the one
# running this script: the terminal, TLS root and socket crates differ between
# Linux, macOS and Windows, so a notices file generated on Linux alone would
# ship in the Windows archive incomplete. For each release target,
# `cargo tree -e normal` names what the apic binary links (build scripts'
# and tests' dependencies are not in it) and `cargo metadata` says where
# each crate's source was unpacked.
#
# Every licence file a crate ships is reproduced (a crate under "MIT OR
# Apache-2.0" ships both texts), and any NOTICE file too (Apache License 2.0
# section 4(d) requires it). Each crate also gets the URL of its exact
# version on crates.io, where its source can be downloaded. The Rust standard
# library, compiled into every binary, comes first.
#
# Usage: scripts/notices.sh [output-file]
# Run from the repository root. NOTICES_PACKAGE names another workspace
# package to list instead of apic (for trying the script out).
# Exits non-zero if a crate has no licence file, so a release cannot ship
# notices with a hole in them.
set -eu
out="${1:-THIRD_PARTY_NOTICES.md}"

command -v python3 >/dev/null 2>&1 || { echo "scripts/notices.sh needs python3" >&2; exit 1; }

NOTICES_OUT="$out" NOTICES_PACKAGE="${NOTICES_PACKAGE:-apic}" exec python3 - <<'PY'
import json
import os
import re
import subprocess
import sys

OUT = os.environ["NOTICES_OUT"]
PACKAGE = os.environ["NOTICES_PACKAGE"]

# The release targets, as .goreleaser.yaml builds them.
TARGETS = [
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-gnu",
    "aarch64-pc-windows-gnullvm",
]

# Crates known to publish no licence file in their crates.io package, each
# with where its licence text is. A crate goes here only after someone has
# read that text; any other crate without a licence file fails the script.
NO_LICENCE_FILE = {
    # "example-crate": "https://github.com/example/example-crate/blob/main/LICENSE",
}

LICENCE_FILE = re.compile(r"^(licen[cs]e|copying|unlicense|copyright)([-_.].*)?$", re.I)
NOTICE_FILE = re.compile(r"^notice([-_.].*)?$", re.I)


def run(*args):
    done = subprocess.run(args, stdout=subprocess.PIPE, text=True)
    if done.returncode != 0:
        sys.exit("scripts/notices.sh: %s failed" % " ".join(args))
    return done.stdout


def fenced(path):
    with open(path, encoding="utf-8", errors="replace") as f:
        text = f.read()
    if not text.endswith("\n"):
        text += "\n"
    # A fence longer than any run of backticks in the text itself.
    longest = max((len(m) for m in re.findall(r"`+", text)), default=0)
    fence = "`" * max(3, longest + 1)
    return fence + "\n" + text + fence + "\n"


# name, version -> package, for every crate that comes from a registry or a
# git repository. Workspace members and path dependencies are apic itself.
third_party = {}
linked = set()
for target in TARGETS:
    meta = json.loads(run("cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", target))
    for pkg in meta["packages"]:
        if pkg.get("source"):
            third_party[(pkg["name"], pkg["version"])] = pkg
    tree = run("cargo", "tree", "--locked", "-p", PACKAGE, "-e", "normal", "--target", target,
               "--prefix", "none", "--format", "{p}")
    for line in tree.splitlines():
        m = re.match(r"^(\S+) v(\S+)", line)
        if m:
            linked.add((m.group(1), m.group(2)))

crates = sorted(key for key in linked if key in third_party)

lines = []
missing = []


def emit(text=""):
    lines.append(text)


emit("# Third-party notices")
emit()
emit("apic is built with Rust from the following open source crates. Each is used under its own licence, reproduced below.")
emit()
emit("apic itself is released under the MIT Licence; see LICENSE.")
emit()

# The standard library's licence texts come with the toolchain: the two
# licences it is offered under, and the copyright notices of the library and
# of the crates built into it.
rustc = run("rustc", "--version").split()[1]
sysroot = run("rustc", "--print", "sysroot").strip()
std_docs = os.path.join(sysroot, "share", "doc", "rust")
std_files = ["licenses/MIT.txt", "licenses/Apache-2.0.txt", "COPYRIGHT-library.html"]
emit("## Rust standard library %s" % rustc)
emit()
emit("Source: https://github.com/rust-lang/rust/tree/%s/library" % rustc)
emit()
emit("Licence: MIT OR Apache-2.0")
emit()
for name in std_files:
    path = os.path.join(std_docs, *name.split("/"))
    if not os.path.isfile(path):
        emit("_%s not found in the toolchain._" % name)
        emit()
        missing.append("the Rust standard library (no %s)" % path)
        continue
    emit("### %s" % os.path.basename(name))
    emit()
    emit(fenced(path))

for name, version in crates:
    pkg = third_party[(name, version)]
    directory = os.path.dirname(pkg["manifest_path"])
    emit("## %s %s" % (name, version))
    emit()
    emit("Source: https://crates.io/crates/%s/%s" % (name, version))
    emit()
    emit("Licence: %s" % (pkg.get("license") or "see below"))
    emit()
    files = sorted(os.listdir(directory))
    licences = [f for f in files if LICENCE_FILE.match(f) and os.path.isfile(os.path.join(directory, f))]
    # The file Cargo.toml names, when it is not one of the usual names.
    named = pkg.get("license_file")
    if named and os.path.basename(named) not in licences:
        path = os.path.normpath(os.path.join(directory, named))
        if os.path.isfile(path):
            licences.append(named)
    notices = [f for f in files if NOTICE_FILE.match(f) and os.path.isfile(os.path.join(directory, f))]
    if not licences:
        if name in NO_LICENCE_FILE:
            emit("This crate's package carries no licence file. Its licence text is at %s" % NO_LICENCE_FILE[name])
            emit()
        else:
            emit("_No licence file found in the crate; see the crate's repository._")
            emit()
            missing.append("%s %s" % (name, version))
        continue
    for f in licences + notices:
        emit("### %s" % os.path.basename(f))
        emit()
        emit(fenced(os.path.join(directory, f)))

with open(OUT, "w", encoding="utf-8") as f:
    f.write("\n".join(lines).rstrip("\n") + "\n")

print("wrote %s (%d crates)" % (OUT, len(crates)))
if missing:
    print("no licence file found for: " + ", ".join(missing), file=sys.stderr)
    sys.exit(1)
PY
