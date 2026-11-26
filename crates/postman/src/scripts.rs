//! Turning the trivial forms of `pm.test` scripts into assertions and
//! captures.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

/// What a test script contributed to a request.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Translated {
    pub(crate) asserts: Vec<String>,
    pub(crate) captures: Vec<String>,
    /// One line each: the script line that was not translated.
    pub(crate) unsupported: Vec<String>,
}

fn pattern(p: &str) -> Regex {
    Regex::new(p).expect("a valid pattern")
}

// The classes are spelt out: a word character is an ASCII letter, digit
// or underscore, and white space is ASCII white space.
/// A whole pm.test on one line, the way Postman's snippets and most
/// hand-written tests put a single check: the body is an arrow expression
/// or a braced block.
static TEST_INLINE: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r#"^pm\.test\([\t\n\f\r ]*(?:"[^"]*"|'[^']*'|`[^`]*`)[\t\n\f\r ]*,[\t\n\f\r ]*(?:(?:function[\t\n\f\r ]*\([\t\n\f\r ]*\)|\([\t\n\f\r ]*\)[\t\n\f\r ]*=>)[\t\n\f\r ]*\{(.*)\}|\([\t\n\f\r ]*\)[\t\n\f\r ]*=>[\t\n\f\r ]*(.+?))[\t\n\f\r ]*\)[\t\n\f\r ]*;?$"#,
    )
});
static TEST_OPEN: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r#"^pm\.test\(.*,[\t\n\f\r ]*(?:function[\t\n\f\r ]*\([\t\n\f\r ]*\)|\([\t\n\f\r ]*\)[\t\n\f\r ]*=>)[\t\n\f\r ]*\{[\t\n\f\r ]*$"#,
    )
});
static BLOCK_CLOSE: LazyLock<Regex> =
    LazyLock::new(|| pattern(r#"^\}[\t\n\f\r ]*\)?[\t\n\f\r ]*;?$"#));
static JSON_VAR: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r#"^(?:var|let|const)[\t\n\f\r ]+([0-9A-Za-z_]+)[\t\n\f\r ]*=[\t\n\f\r ]*pm\.response\.json\(\)[\t\n\f\r ]*;?$"#,
    )
});
static STATUS: LazyLock<Regex> =
    LazyLock::new(|| pattern(r#"^pm\.response\.to\.have\.status\(([0-9]+)\)[\t\n\f\r ]*;?$"#));
static STATUS_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    pattern(r#"^pm\.response\.to\.have\.status\(["']([^"']+)["']\)[\t\n\f\r ]*;?$"#)
});
static CODE: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r#"^pm\.expect\(pm\.response\.code\)\.to\.(?:eql|equal|be\.equal)\(([0-9]+)\)[\t\n\f\r ]*;?$"#,
    )
});
static TIME: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r#"^pm\.expect\(pm\.response\.responseTime\)\.to\.be\.below\(([0-9]+)\)[\t\n\f\r ]*;?$"#,
    )
});
static HEADER_HAS: LazyLock<Regex> = LazyLock::new(|| {
    pattern(r#"^pm\.response\.to\.have\.header\(["']([^"']+)["']\)[\t\n\f\r ]*;?$"#)
});
static HEADER_IS: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r#"^pm\.expect\(pm\.response\.headers\.get\(["']([^"']+)["']\)\)\.to\.(eql|equal|include|contain)\((.+)\)[\t\n\f\r ]*;?$"#,
    )
});
static TEXT_HAS: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r#"^pm\.expect\(pm\.response\.text\(\)\)\.to\.(?:include|contain)\((.+)\)[\t\n\f\r ]*;?$"#,
    )
});
static JSON_IS: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r#"^pm\.expect\(([0-9A-Za-z_]+)((?:\.[0-9A-Za-z_$]+|\[[^\]]+\])+)\)\.to\.(eql|equal|be\.equal|include|contain|exist|be\.true|be\.false)(?:\((.*)\))?[\t\n\f\r ]*;?$"#,
    )
});
static JSON_INLINE: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r#"^pm\.expect\(pm\.response\.json\(\)((?:\.[0-9A-Za-z_$]+|\[[^\]]+\])+)\)\.to\.(eql|equal|be\.equal|include|contain|exist|be\.true|be\.false)(?:\((.*)\))?[\t\n\f\r ]*;?$"#,
    )
});
static SET_FROM_JSON: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r#"^pm\.(?:environment|collectionVariables|globals|variables)\.set\(["']([^"']+)["'],[\t\n\f\r ]*([0-9A-Za-z_]+)((?:\.[0-9A-Za-z_$]+|\[[^\]]+\])+)\)[\t\n\f\r ]*;?$"#,
    )
});
static SET_INLINE: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r#"^pm\.(?:environment|collectionVariables|globals|variables)\.set\(["']([^"']+)["'],[\t\n\f\r ]*pm\.response\.json\(\)((?:\.[0-9A-Za-z_$]+|\[[^\]]+\])+)\)[\t\n\f\r ]*;?$"#,
    )
});
static SET_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r#"^pm\.(?:environment|collectionVariables|globals|variables)\.set\(["']([^"']+)["'],[\t\n\f\r ]*pm\.response\.headers\.get\(["']([^"']+)["']\)\)[\t\n\f\r ]*;?$"#,
    )
});

/// Reads the lines of a `test` script and turns the forms it recognises
/// into assertions and captures. Everything else is reported.
pub(crate) fn translate_tests(lines: &[String]) -> Translated {
    let mut out = Translated::default();
    let mut json_vars: HashSet<String> = HashSet::new();
    for raw in expand_inline_tests(lines) {
        let line = raw.trim();
        if line.is_empty()
            || line.starts_with("//")
            || line.starts_with("/*")
            || line.starts_with('*')
            || TEST_OPEN.is_match(line)
            || BLOCK_CLOSE.is_match(line)
        {
            continue;
        }
        if let Some(m) = JSON_VAR.captures(line) {
            json_vars.insert(m[1].to_string());
            continue;
        }
        if let Some(m) = STATUS.captures(line) {
            out.asserts.push(format!("status == {}", &m[1]));
            continue;
        }
        if let Some(m) = STATUS_TEXT.captures(line) {
            out.asserts.push(format!("statusText == {}", &m[1]));
            continue;
        }
        if let Some(m) = CODE.captures(line) {
            out.asserts.push(format!("status == {}", &m[1]));
            continue;
        }
        if let Some(m) = TIME.captures(line) {
            out.asserts.push(format!("duration < {}", &m[1]));
            continue;
        }
        if let Some(m) = HEADER_HAS.captures(line) {
            out.asserts
                .push(format!("header.{} exists", m[1].to_lowercase()));
            continue;
        }
        if let Some(m) = HEADER_IS.captures(line) {
            out.asserts.push(format!(
                "header.{} {} {}",
                m[1].to_lowercase(),
                chai_op(&m[2]),
                literal(&m[3])
            ));
            continue;
        }
        if let Some(m) = TEXT_HAS.captures(line) {
            out.asserts
                .push(format!("body contains {}", literal(&m[1])));
            continue;
        }
        let group = |m: &regex::Captures<'_>, i: usize| -> String {
            m.get(i).map_or("", |g| g.as_str()).to_string()
        };
        if let Some(m) = JSON_INLINE.captures(line) {
            if let Some(a) = json_assert(&m[1], &m[2], &group(&m, 3)) {
                out.asserts.push(a);
                continue;
            }
        }
        if let Some(m) = JSON_IS.captures(line) {
            if json_vars.contains(&m[1]) {
                if let Some(a) = json_assert(&m[2], &m[3], &group(&m, 4)) {
                    out.asserts.push(a);
                    continue;
                }
            }
        }
        if let Some(m) = SET_INLINE.captures(line) {
            out.captures.push(format!("{} = body.${}", &m[1], &m[2]));
            continue;
        }
        if let Some(m) = SET_FROM_JSON.captures(line) {
            if json_vars.contains(&m[2]) {
                out.captures.push(format!("{} = body.${}", &m[1], &m[3]));
                continue;
            }
        }
        if let Some(m) = SET_HEADER.captures(line) {
            out.captures
                .push(format!("{} = header.{}", &m[1], m[2].to_lowercase()));
            continue;
        }
        out.unsupported.push(line.to_string());
    }
    out
}

/// Replaces a one-line pm.test with the statements inside it, so
/// `pm.test("ok", () => pm.response.to.have.status(200));` reads the same
/// as the three-line form. A statement it then cannot translate is
/// reported on its own.
fn expand_inline_tests(lines: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for raw in lines {
        let Some(m) = TEST_INLINE.captures(raw.trim()) else {
            out.push(raw.clone());
            continue;
        };
        match m.get(2).map(|g| g.as_str()).filter(|s| !s.is_empty()) {
            Some(expression) => out.push(expression.to_string()),
            None => {
                let block = m.get(1).map_or("", |g| g.as_str());
                out.extend(
                    block
                        .split(';')
                        .map(str::trim)
                        .filter(|stmt| !stmt.is_empty())
                        .map(str::to_string),
                );
            }
        }
    }
    out
}

/// Maps a chai expectation on a JSON path onto an assertion.
fn json_assert(path: &str, op: &str, arg: &str) -> Option<String> {
    let sel = format!("body.${path}");
    match op {
        "eql" | "equal" | "be.equal" => Some(format!("{sel} == {}", literal(arg))),
        "include" | "contain" => Some(format!("{sel} contains {}", literal(arg))),
        "exist" => Some(format!("{sel} exists")),
        "be.true" => Some(format!("{sel} == true")),
        "be.false" => Some(format!("{sel} == false")),
        _ => None,
    }
}

fn chai_op(op: &str) -> &'static str {
    match op {
        "include" | "contain" => "contains",
        _ => "==",
    }
}

/// Strips the quotes off a JavaScript string literal; numbers, booleans
/// and anything else pass through.
fn literal(s: &str) -> &str {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() >= 2 {
        let q = b[0];
        if (q == b'"' || q == b'\'' || q == b'`') && b[b.len() - 1] == q {
            return &s[1..s.len() - 1];
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn translate_tests_block_form() {
        let got = translate_tests(&lines(&[
            r#"pm.test("status", function () {"#,
            r#"  pm.response.to.have.status(201);"#,
            r#"  pm.response.to.have.status("Created");"#,
            r#"  const body = pm.response.json();"#,
            r#"  pm.expect(body.items[0]["k.y"]).to.equal(3);"#,
            r#"  pm.expect(body.ok).to.be.true;"#,
            r#"  pm.expect(body.name).to.include('ali');"#,
            r#"  pm.expect(pm.response.text()).to.include("hello");"#,
            r#"  pm.expect(pm.response.headers.get('Location')).to.eql("/todos/1");"#,
            r#"  pm.collectionVariables.set("id", body.id);"#,
            r#"  pm.globals.set("loc", pm.response.headers.get("Location"));"#,
            r#"  pm.environment.set("tok", pm.response.json().token);"#,
            r#"  pm.expect(other.x).to.eql(1);"#,
            r#"  // a comment"#,
            r#"});"#,
        ]));
        assert_eq!(
            got.asserts,
            [
                "status == 201",
                "statusText == Created",
                r#"body.$.items[0]["k.y"] == 3"#,
                "body.$.ok == true",
                "body.$.name contains ali",
                "body contains hello",
                "header.location == /todos/1"
            ]
        );
        assert_eq!(
            got.captures,
            [
                "id = body.$.id",
                "loc = header.location",
                "tok = body.$.token"
            ]
        );
        assert_eq!(got.unsupported, ["pm.expect(other.x).to.eql(1);"]);
    }

    // A pm.test on one line reads the same as the block form: an arrow
    // expression, an arrow block or a function block, each statement
    // inside translated or reported on its own.
    #[test]
    fn translate_one_line_tests() {
        let got = translate_tests(&lines(&[
            r#"pm.test("ok", () => pm.response.to.have.status(200));"#,
            r#"pm.test('fast', function () { pm.expect(pm.response.responseTime).to.be.below(500); });"#,
            "pm.test(`both`, () => { pm.response.to.have.header(\"ETag\"); pm.expect(pm.response.text()).to.include(\"x\") })",
            r#"pm.test("custom", () => pm.expect(tv4.validate(1, 2)).to.be.true);"#,
        ]));
        assert_eq!(
            got.asserts,
            [
                "status == 200",
                "duration < 500",
                "header.etag exists",
                "body contains x"
            ]
        );
        assert_eq!(
            got.unsupported,
            ["pm.expect(tv4.validate(1, 2)).to.be.true"]
        );
    }
}
