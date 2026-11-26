use std::path::{Path, PathBuf};

use apic_httpfile::{directive_order, format, format_request, parse};

const MANIFEST: &str = env!("CARGO_MANIFEST_DIR");

#[test]
fn format_golden() {
    let dir = Path::new(MANIFEST).join("testdata/fmt");
    let mut ins: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(".in.http"))
        .collect();
    ins.sort();
    assert!(!ins.is_empty(), "no golden pairs");
    for input in ins {
        let name = input.to_string_lossy().into_owned();
        let src = std::fs::read_to_string(&input).unwrap();
        let want = std::fs::read_to_string(
            name.strip_suffix(".in.http").unwrap().to_string() + ".out.http",
        )
        .unwrap();
        let got = format(&src);
        // A checkout with autocrlf turns the committed LF golden file into
        // CRLF; the formatter's output is always LF.
        assert_eq!(got, want.replace("\r\n", "\n"), "{name}");
        assert_eq!(format(&got), got, "{name}: not idempotent");
        // The canonical form parses to the same requests.
        let (before, _) = parse(&name, &src);
        let (after, _) = parse(&name, &got);
        assert_eq!(before.requests.len(), after.requests.len(), "{name}");
        for (i, (b, a)) in before.requests.iter().zip(&after.requests).enumerate() {
            assert!(
                b.name == a.name
                    && b.method == a.method
                    && b.url == a.url
                    && b.headers.len() == a.headers.len()
                    && b.directives.len() == a.directives.len()
                    && b.body_file == a.body_file
                    && (b.body == a.body || is_json(&b.body)),
                "{name}: request {i} changed meaning: {b:?} -> {a:?}"
            );
        }
    }
}

/// Reports whether a body is one JSON document, the only kind the
/// formatter rewrites.
fn is_json(body: &str) -> bool {
    let t = body.trim();
    (t.starts_with('{') || t.starts_with('[')) && !t.contains("{{")
}

fn http_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            http_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "http")
            && !path.to_string_lossy().contains("/fmt/")
        {
            out.push(path);
        }
    }
}

/// Every .http file in the repository formats to itself once formatted,
/// and keeps its requests: the formatter must be safe to run on real
/// projects.
#[test]
fn format_is_idempotent_on_repository_files() {
    let mut files = Vec::new();
    for root in ["../../examples", "../demoapi/project", "testdata"] {
        http_files(&Path::new(MANIFEST).join(root), &mut files);
    }
    assert!(files.len() >= 5, "found only {files:?}");
    for f in files {
        let name = f.to_string_lossy();
        let src = std::fs::read_to_string(&f).unwrap();
        let once = format(&src);
        assert_eq!(format(&once), once, "{name}: not idempotent");
        let (before, _) = parse(&name, &src);
        let (after, _) = parse(&name, &once);
        assert_eq!(
            before.requests.len(),
            after.requests.len(),
            "{name}: requests before and after formatting"
        );
    }
    assert!(
        format("").is_empty() && format("\n\n").is_empty(),
        "empty input stays empty"
    );
    assert_eq!(format("GET http://x\r\n"), "GET http://x\n");
}

#[test]
fn format_request_by_name_and_number() {
    let src = "@base = https://x\n\n###  a\n# @assert status == 200\n# @name a\nGET {{base}}/a\n\n\n### b\n# @name   b\nGET   {{base}}/b\n";
    assert_eq!(
        format_request(src, "b").as_deref(),
        Some("@base = https://x\n\n###  a\n# @assert status == 200\n# @name a\nGET {{base}}/a\n\n\n### b\n# @name b\nGET {{base}}/b\n")
    );
    assert_eq!(
        format_request(src, "1").as_deref(),
        Some("@base = https://x\n\n### a\n# @name a\n# @assert status == 200\nGET {{base}}/a\n\n### b\n# @name   b\nGET   {{base}}/b\n")
    );
    assert!(
        format_request(src, "c").is_none(),
        "an unknown name should not match"
    );
    assert!(
        format_request(src, "3").is_none(),
        "a number past the last block should not match"
    );
    // The implicit block is request 1 when it holds a request line, a
    // `###` block without one is not counted, and the number is what the
    // parser gives the request.
    let implicit =
        "# @name  p\nGET   https://x/p\n\n### heading only\n\n### q\n# @name  q\nGET https://x/q\n";
    let (f, _) = parse("t.http", implicit);
    assert!(
        f.requests.len() == 2 && f.requests[1].name == "q" && f.requests[1].index == 2,
        "parser: {:?}",
        f.requests
    );
    let got = format_request(implicit, "p").unwrap();
    assert!(
        got.starts_with("# @name p\nGET https://x/p\n\n### heading only\n"),
        "implicit by name:\n{got}"
    );
    let got = format_request(implicit, "2").unwrap();
    assert!(
        got.ends_with("### q\n# @name q\nGET https://x/q\n") && got.starts_with("# @name  p\n"),
        "second request, not second block:\n{got}"
    );
    assert!(
        format_request(implicit, "3").is_none(),
        "a heading block is not a request"
    );
    // Formatting every request one by one is the whole-file format.
    let both = format_request(src, "a").unwrap();
    let both = format_request(&both, "b").unwrap();
    assert_eq!(both, format(src));
}

/// A CRLF file keeps its CRLF outside the formatted block, byte for byte,
/// and the formatted block takes the file's line ending.
#[test]
fn format_request_keeps_crlf() {
    let src = "###  a\r\n# @name   a\r\nGET https://x/a\r\n\r\n### b\r\n# @name   b\r\nGET https://x/b\r\n";
    assert_eq!(
        format_request(src, "b").as_deref(),
        Some("###  a\r\n# @name   a\r\nGET https://x/a\r\n\r\n### b\r\n# @name b\r\nGET https://x/b\r\n")
    );
    assert_eq!(
        format_request(src, "a").as_deref(),
        Some("### a\r\n# @name a\r\nGET https://x/a\r\n\r\n### b\r\n# @name   b\r\nGET https://x/b\r\n")
    );
}

#[test]
fn directive_order_is_the_formatters() {
    let order = directive_order();
    assert_eq!(order.len(), 15);
    assert_eq!((order[0], order[14]), ("name", "capture"));
    let src = format!(
        "{}\nGET http://x\n",
        order
            .iter()
            .rev()
            .map(|k| format!("# @{k} v"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let want = format!(
        "{}\nGET http://x\n",
        order
            .iter()
            .map(|k| format!("# @{k} v"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert_eq!(format(&src), want);
}

#[test]
fn format_headers_and_json_bodies() {
    let src = "POST   http://x   HTTP/1.1\ncontent-TYPE:   application/json\nx-api_key: k  \n\n{\"b\":1.0,\"a\":[],\"o\":{ },\"n\":[1,{\"k\":null}]}\n\n\n>> out.json\n";
    assert_eq!(
        format(src),
        "POST http://x HTTP/1.1\nContent-Type: application/json\nX-Api_key: k\n\n{\n  \"b\": 1.0,\n  \"a\": [],\n  \"o\": {},\n  \"n\": [\n    1,\n    {\n      \"k\": null\n    }\n  ]\n}\n\n>> out.json\n"
    );
    // A body with placeholders, or under another content type, is the
    // author's byte for byte.
    for src in [
        "POST http://x\n\n{\"a\":{{a}}}\n",
        "POST http://x\nContent-Type: text/plain\n\n{\"a\":1}\n",
        "POST http://x\n\n{not json}\n",
    ] {
        assert_eq!(format(src), src);
    }
}
