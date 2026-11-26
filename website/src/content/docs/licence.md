---
title: "Licence"
description: "apic is MIT licensed; what that lets you do, and the licences of the crates it ships with."
---
apic is released under the **MIT Licence**. The full text lives in
[`LICENSE`](https://github.com/hungovercoders/apic-rust/blob/main/LICENSE) in the
repository, and a copy ships inside every release archive and package and in
the container image.

In short: use it, change it, ship it inside your own products, commercial or
not. Keep the copyright notice with any substantial copy of the source, and
understand that it comes with no warranty.

## Third-party code

apic is a single static binary, so the Rust standard library and the
open source crates it is built from travel inside it. Each keeps its own
licence, and all of them are permissive:

| Licence | What |
|---|---|
| MIT or Apache-2.0, at your choice | Rust's standard library and most crates, including `clap`, `serde`, `serde_json`, `regex`, `url`, `uuid`, `base64`, `flate2` and the hashes (`sha2`, `md-5`, `hmac`) |
| MIT | `hyper`, `tokio`, `h2`, `serde_yaml_ng` and `unsafe-libyaml`, among others |
| Apache-2.0, ISC or MIT, at your choice | the TLS stack itself: `rustls`, `rustls-native-certs` and `rustls-pemfile` |
| ISC, or Apache-2.0 and ISC | the TLS stack's certificate verification (`rustls-webpki`, `untrusted`) and its cryptography (`ring`) |
| BSD-3-Clause | `subtle` (constant-time comparison, under the TLS stack) |
| Unicode-3.0 | the ICU crates `url` uses for international domain names, which carry Unicode data tables |

A few crates add a further choice to MIT or Apache-2.0 (the Unlicense for
`memchr` and `aho-corasick`, Zlib for `miniz_oxide`, 0BSD for `adler2`,
BSL-1.0 for `ryu`); apic takes each under MIT or Apache-2.0.

Nothing here is copyleft, so none of it reaches your use of apic or your own
code. The build enforces the list: a crate under any other licence fails CI
before it can ship.

`THIRD_PARTY_NOTICES.md` reproduces each licence in full, along with the
`NOTICE` files some crates ship, and links each crate's exact version on
crates.io, where its source is. Every release archive and package contains it,
and the container image has it in `/usr/share/doc/apic` beside `LICENSE` and
`CA-CERTIFICATES.md`. The image's CA bundle is Mozilla's certificate store
(MPL-2.0) as Alpine packages it, and that file names the package and its
source. Regenerate the notices from a checkout at any time:

```sh
task notices
```

The generator walks the crate set for **every** released platform, so the
notices are complete whichever archive you downloaded.

The VS Code extension is MIT too. It bundles a handful of npm packages (the
language client and what it pulls in, under MIT, ISC and BlueOak-1.0.0), and
every `.vsix` carries their notices.
