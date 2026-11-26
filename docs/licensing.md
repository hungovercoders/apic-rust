# Licensing

- apic is MIT (`LICENSE`, root). Keep it there: CI's `licences` job fails
  without it, and goreleaser ships it in every archive.
- `task notices` (`scripts/notices.sh`) regenerates
  `THIRD_PARTY_NOTICES.md`, which is generated rather than committed (it is
  in `.gitignore`); goreleaser runs it before packaging. It starts with Rust itself (the standard
  library is in every binary, under MIT or Apache-2.0, with the copyright
  notices the toolchain ships for it) and gives every crate the URL of its
  exact version on crates.io, where its source is. CI's `licences` job
  checks both.
- Every way apic ships carries `LICENSE` and the notices: the archives, the
  deb, rpm and apk packages (`/usr/share/doc/apic`), and the image (also
  `/usr/share/doc/apic`, through `extra_files` in `dockers_v2`). The image
  also ships Alpine's CA bundle, Mozilla's certificate store under MPL-2.0,
  so the Dockerfile writes `CA-CERTIFICATES.md` naming the package version
  and its source, and the image's `org.opencontainers.image.licenses` label
  lists every licence inside it. CI's `image` job checks the three files are
  there.
- The generator unions the crate set across every released target triple,
  not just the host: the terminal, TLS root and socket crates differ between
  Linux, macOS and Windows. It lists what `cargo tree -e normal` says the
  binary links, so build scripts' and tests' dependencies are left out. It
  reproduces every licence file a crate ships (both texts of an "MIT OR
  Apache-2.0" crate) and each `NOTICE` (required by Apache-2.0 4(d)), and
  exits non-zero if a crate has no licence file at all. A crate known to
  publish none goes in the script's `NO_LICENCE_FILE` table, with where its
  licence text is, only after someone has read that text.
- `deny.toml` is the permissive list in SPDX ids, and `cargo deny check
  licenses` (part of `task licences` and of CI's `licences` job) fails on a
  crate whose licence is not on it, for any release target.
- The VS Code extension bundles its runtime npm dependencies (today
  `vscode-languageclient` and what it pulls in) into `dist/extension.js`;
  `npm run package` runs `editors/vscode/notices.mjs` first, which writes the
  extension's own notices file (generated, gitignored) and fails on a package
  with no licence file. The same permissive-only rule applies there.
- Keep new dependencies permissive (MIT, BSD, Apache-2.0, ISC, Zlib and
  Unicode-3.0 for Unicode data tables: the list in `deny.toml`). Third-party
  versions live in one place, `[workspace.dependencies]` in the root
  `Cargo.toml`. Anything reciprocal —
  GPL or LGPL — would change apic's own terms, so it is off the table for a
  statically linked binary.
- The rule is about what ships: the binary, the archives, the image and the
  extension. Tooling that only runs in CI, such as the npm devDependencies
  slopstopper's browser checks use (`axe-core` is MPL-2.0), is not
  distributed and is outside it.
- Write the AWS signer and the OpenAPI reader style of code from the spec,
  not by copying from another project; the tree carries no third-party
  source files and should stay that way.
