use std::fmt::Write as _;

use apic_httpfile::Header;
use apic_runner::{Description, Progress, Result, VarInfo, BASE64, MASKED};

use crate::jsonhl::highlight_json;
use crate::output::{is_json, pretty_json};
use crate::style::{self, rounded_border, Style};
use crate::theme::Theme;

/// Controls how a result is rendered for humans.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// Request headers and body, response headers.
    pub verbose: bool,
    /// Where long values are cut; 0 means 80.
    pub width: usize,
}

/// Renders "METHOD url" with the URL masked when redacting.
pub fn request_line(t: &Theme, res: &Result) -> String {
    t.method(&res.request.method) + " " + &t.url.render(&res.request.display_url(res.redact))
}

/// Renders the request headers and body (verbose mode).
pub fn request_detail(t: &Theme, res: &Result) -> String {
    let mut b = String::new();
    for h in res.request.display_headers(res.redact) {
        let _ = writeln!(
            b,
            "{} {}",
            t.header.render(&format!("{}:", h.name)),
            h.value
        );
    }
    if let Some(tls) = &res.request.tls {
        let _ = writeln!(b, "{} {}", t.dim.render("tls:"), tls);
    }
    if let Some(proxy) = &res.request.proxy {
        let _ = writeln!(b, "{} {}", t.dim.render("proxy:"), proxy);
    }
    let note = res.auth_note();
    if !note.is_empty() {
        let _ = writeln!(b, "{} {}", t.dim.render("auth:"), note);
    }
    let body = res.request.display_body(res.redact);
    if !body.is_empty() {
        let _ = writeln!(b, "\n{}", body.trim_end_matches('\n'));
    }
    b
}

/// Renders "200 OK · 87 ms · 412 B"; when the request never got a response
/// it renders the errors instead.
pub fn status_line(t: &Theme, res: &Result) -> String {
    if !res.skipped.is_empty() {
        return t.dim.render(&format!("skipped ({})", res.skipped)) + "\n";
    }
    let Some(response) = &res.response else {
        let mut b = String::new();
        for e in &res.errors {
            let _ = writeln!(b, "{} {}", t.fail.render("✗"), e);
        }
        return b;
    };
    let status = match res.raw() {
        Some(raw) => t.status(raw.status, &raw.status_text),
        None => t.status(response.status, &response.status_text),
    };
    let mut line = format!(
        "{} {} {} {} {}",
        status,
        t.dim.render("·"),
        t.latency(response.duration_ms),
        t.dim.render("·"),
        t.dim.render(&size(response.size))
    );
    if res.attempts > 1 {
        line += " ";
        line += &t.dim.render(&format!("· {} attempts", res.attempts));
    }
    line + "\n"
}

/// Renders where the round trip went, when the response carries a
/// breakdown: "dns 12 ms · connect 18 ms · tls 41 ms · ttfb 60 ms · total
/// 87 ms · new connection".
pub fn timings(t: &Theme, res: &Result) -> String {
    let Some(tm) = res.response.as_ref().and_then(|r| r.timings.as_ref()) else {
        return String::new();
    };
    let conn = if tm.reused {
        "reused connection"
    } else {
        "new connection"
    };
    t.dim.render(&format!(
        "dns {} ms · connect {} ms · tls {} ms · ttfb {} ms · total {} ms · {}",
        tm.dns_ms, tm.connect_ms, tm.tls_ms, tm.ttfb_ms, tm.total_ms, conn
    )) + "\n"
}

/// Renders the progress line printed after a failed attempt of a request
/// that is being retried.
pub fn attempt(t: &Theme, p: &Progress) -> String {
    t.dim
        .render(&format!("attempt {}/{} · {}", p.attempt, p.max, p.failure))
        + "\n"
}

/// Renders the response headers sorted and lower-cased, with sensitive
/// values masked and every value masked when redacting. Empty when there
/// is no response.
pub fn response_headers(t: &Theme, res: &Result) -> String {
    let Some(response) = &res.response else {
        return String::new();
    };
    let mut b = String::new();
    for (k, v) in response.display_headers(res.redact) {
        let _ = writeln!(
            b,
            "{} {}",
            t.header.render(&format!("{}:", k.to_lowercase())),
            v
        );
    }
    b
}

/// Pretty-prints and highlights a JSON body; other bodies are returned as
/// they are. The result has no trailing newline.
pub fn render_body(t: &Theme, raw: &[u8]) -> String {
    let pretty = pretty_json(raw);
    let pretty = String::from_utf8_lossy(&pretty);
    let pretty = pretty.trim_end_matches('\n');
    if is_json(raw) {
        return highlight_json(t, pretty);
    }
    pretty.to_string()
}

/// Renders the assertions, captures and errors of a result. With
/// `expected` set, failed assertions show the expected value as well as
/// the actual one. A `width` of 0 means 80.
pub fn checks(t: &Theme, res: &Result, width: usize, expected: bool) -> String {
    let width = if width == 0 { 80 } else { width };
    let mut b = String::new();
    for a in res.display_asserts() {
        if !a.error.is_empty() {
            let _ = writeln!(
                b,
                "{} {} {}",
                t.fail.render("✗"),
                a.expr,
                t.dim.render(&format!("({})", a.error))
            );
        } else if a.pass {
            let _ = writeln!(b, "{} {}", t.ok.render("✓"), a.expr);
        } else {
            let _ = writeln!(
                b,
                "{} {} {}",
                t.fail.render("✗"),
                a.expr,
                t.dim
                    .render(&format!("(actual: {})", truncate(&a.actual, width)))
            );
            if expected {
                let _ = writeln!(
                    b,
                    "  {} {}",
                    t.dim.render("expected:"),
                    truncate(&a.expected, width)
                );
            }
        }
    }
    if !res.saved_to.is_empty() {
        let _ = writeln!(
            b,
            "{} {}",
            t.capture.render("↳"),
            t.dim.render("saved to ") + &res.saved_to
        );
    }
    for (n, v) in res.display_captures() {
        let _ = writeln!(
            b,
            "{} {} = {}",
            t.capture.render("↳"),
            n,
            truncate(&v, width)
        );
    }
    if res.response.is_some() {
        for e in &res.errors {
            let _ = writeln!(b, "{} {}", t.fail.render("✗"), e);
        }
    }
    for w in &res.warnings {
        let _ = writeln!(b, "{} {}", t.warn.render("!"), w);
    }
    b
}

/// Renders the whole report for one request, as `apic run` prints it.
pub fn result(t: &Theme, res: &Result, o: Options) -> String {
    let mut b = String::new();
    b += &(request_line(t, res) + "\n");
    if res.dry_run {
        // The request is the whole point of a dry run, so its headers and
        // body are shown without -v, and so is what a run would fill in.
        b += &request_detail(t, res);
        for w in &res.warnings {
            let _ = writeln!(b, "{} {}", t.warn.render("!"), w);
        }
        b += &(t.dim.render("dry run · not sent") + "\n");
        return b;
    }
    if o.verbose {
        b += &request_detail(t, res);
        b += "\n";
    }
    if let (true, Some(response)) = (o.verbose, &res.response) {
        if !response.proto.is_empty() {
            // Which protocol the server answered over, in front of the
            // status.
            b += &(t.dim.render(&response.proto) + " ");
        }
    }
    b += &status_line(t, res);
    let Some(response) = &res.response else {
        return b;
    };
    if o.verbose {
        b += &timings(t, res);
        b += &response_headers(t, res);
    }
    let body = res.display_raw_body();
    if !body.is_empty() {
        b += &format!("\n{}\n", body_or_summary(t, res, &body));
        if res
            .display_response()
            .is_some_and(|shown| shown.body_truncated)
        {
            let mut note = format!(
                "… {} of {} shown (--body-limit)",
                size(body.len()),
                size(response.size)
            );
            if res.recorded {
                note += &format!(
                    "; apic select {} body.$.<path> reads the rest",
                    res.record_key
                );
            } else if !res.unrecorded.is_empty() {
                note += "; the rest is not kept: ";
                note += &res.unrecorded;
            }
            b += &(t.dim.render(&note) + "\n");
        }
    }
    let checks = checks(t, res, o.width, false);
    if !checks.is_empty() {
        b += "\n";
        b += &checks;
    }
    b
}

/// Renders a body, or for one that is not text, a line with its size and
/// content type instead of the bytes.
pub fn body_or_summary(t: &Theme, res: &Result, body: &[u8]) -> String {
    let Some(response) = res.response.as_ref().filter(|r| r.body_encoding == BASE64) else {
        return render_body(t, body);
    };
    let ct = response
        .headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.as_str())
        .next_back()
        .unwrap_or("unknown content type");
    t.dim.render(&format!(
        "(binary body · {} · {}; save it with >> file or --output)",
        size(body.len()),
        ct
    ))
}

/// Renders one row per request followed by the totals line, for flows of
/// more than one request.
pub fn summary_table(t: &Theme, results: &[Result]) -> String {
    let mut b = String::new();
    let name_w = results
        .iter()
        .map(|r| style::width(&result_name(r)))
        .max()
        .unwrap_or(0);
    let mut total: i64 = 0;
    for r in results {
        let name = result_name(r);
        let pad = " ".repeat(name_w - style::width(&name));
        let (mut mark, mut status, mut latency) =
            (t.mark(r.ok), t.dim.render("   —"), String::new());
        if !r.skipped.is_empty() {
            mark = t.dim.render("-");
            status = t.dim.render(&format!("skipped ({})", r.skipped));
        }
        if let Some(response) = &r.response {
            status = t.status(response.status, "");
            status.truncate(status.trim_end_matches(' ').len());
            latency = t.latency(response.duration_ms);
            total += response.duration_ms;
        }
        let _ = write!(b, "{mark} {name}{pad}  {status}  {latency}");
        if !r.ok {
            let why = first_problem(r);
            if !why.is_empty() {
                let _ = write!(b, "  {}", t.dim.render(&why));
            }
        }
        b += "\n";
    }
    b += "\n";
    b += &summary_line(t, results);
    b += &t
        .dim
        .render(&format!(" · {} requests · {} ms", results.len(), total));
    b += "\n";
    b
}

/// Renders "3 passed", "1 failed, 3 passed" or "3 passed, 1 skipped".
pub fn summary_line(t: &Theme, results: &[Result]) -> String {
    let skipped = results.iter().filter(|r| !r.skipped.is_empty()).count();
    let passed = results
        .iter()
        .filter(|r| r.skipped.is_empty() && r.ok)
        .count();
    let failed = results.len() - passed - skipped;
    let mut line = t.ok.render(&format!("{passed} passed"));
    if failed > 0 {
        line = t.fail.render(&format!("{failed} failed")) + ", " + &format!("{passed} passed");
    }
    if skipped > 0 {
        line += ", ";
        line += &t.dim.render(&format!("{skipped} skipped"));
    }
    line
}

/// The name a result goes by in a summary: the request's name, or where
/// it is in its file.
pub(crate) fn result_name(r: &Result) -> String {
    if !r.request.name.is_empty() {
        return r.request.name.clone();
    }
    format!("{}:{}", r.request.file, r.request.line)
}

/// The one-line reason a result is not OK.
fn first_problem(r: &Result) -> String {
    for a in r.display_asserts() {
        if !a.error.is_empty() {
            return format!("{} ({})", a.expr, a.error);
        }
        if !a.pass {
            return format!("{} (actual: {})", a.expr, truncate(&a.actual, 40));
        }
    }
    match r.errors.first() {
        Some(e) => truncate(e, 60),
        None => String::new(),
    }
}

/// Renders what `apic describe` shows. `headers` are the request's raw
/// headers in file order.
pub fn describe(t: &Theme, d: &Description, headers: &[Header]) -> String {
    let mut b = String::new();
    let _ = writeln!(
        b,
        "{} {}",
        t.method(&d.method),
        t.url.render(&d.url_template)
    );
    if !d.description.is_empty() {
        b += &d.description;
        b += "\n";
    }
    let _ = writeln!(b, "{} {}:{}", t.dim.render("file:"), d.file, d.line);
    if d.id != d.name {
        let _ = writeln!(b, "{} {}", t.dim.render("id:  "), d.id);
    }
    if d.disabled {
        let _ = writeln!(
            b,
            "{}",
            t.dim.render(&format!(
                "disabled: skipped when its file runs as a flow; apic run {} still sends it",
                d.id
            ))
        );
    }
    if !d.sleep.is_empty() {
        let _ = writeln!(b, "{} {} before sending", t.dim.render("sleep:"), d.sleep);
    }
    if !d.http_version.is_empty() {
        let _ = writeln!(b, "{} {} only", t.dim.render("http: "), d.http_version);
    }
    if !headers.is_empty() {
        b += &section(t, "headers");
        for h in headers {
            let _ = writeln!(
                b,
                "  {} {}",
                t.header.render(&format!("{}:", h.name)),
                h.value
            );
        }
    }
    if !d.body.is_empty() {
        b += &section(t, "body");
        b += &indent(&d.body);
        b += "\n";
    }
    if !d.auth.is_empty() {
        b += &section(t, "auth");
        let _ = writeln!(
            b,
            "  {} {}",
            d.auth,
            t.dim.render(&format!("({})", d.auth_source))
        );
    }
    if !d.body_file.is_empty() {
        b += &section(t, "body file");
        let _ = writeln!(b, "  {}", d.body_file);
    }
    if !d.save_to.is_empty() {
        b += &section(t, "saves the response to");
        let _ = writeln!(b, "  {}", d.save_to);
    }
    if let Some(tls) = &d.tls {
        b += &section(t, "tls");
        let _ = writeln!(b, "  {tls}");
    }
    if let Some(proxy) = &d.proxy {
        b += &section(t, "proxy");
        let _ = writeln!(b, "  {proxy}");
    }
    b += &section(t, "variables");
    if d.variables.is_empty() {
        b += "  (none)\n";
    }
    b += &variables(t, &d.variables, true);
    if !d.steps.is_empty() {
        b += &section(t, "steps");
        for st in &d.steps {
            let _ = writeln!(b, "  {st}");
        }
    }
    if !d.captures.is_empty() {
        b += &section(t, "captures");
        for c in &d.captures {
            let _ = writeln!(b, "  {} {}", t.capture.render("↳"), c);
        }
    }
    if !d.asserts.is_empty() {
        b += &section(t, "asserts");
        for x in &d.asserts {
            let _ = writeln!(b, "  {x}");
        }
    }
    b += "\n";
    b += &readiness(t, d);
    b += "\n";
    b
}

/// Renders the boxed verdict at the end of a description.
pub fn readiness(t: &Theme, d: &Description) -> String {
    let boxed = Style::new().border(rounded_border(), &[]).padding(&[0, 1]);
    if d.ready {
        return boxed
            .border_foreground("42")
            .render(&(t.ok.render("ready") + " " + &t.url.render(&d.url)));
    }
    let missing: Vec<String> = d
        .variables
        .iter()
        .filter(|v| v.missing)
        .map(|v| format!("{{{{{}}}}}", v.name))
        .collect();
    boxed.border_foreground("214").render(
        &(t.warn.render("not ready")
            + " "
            + &t.dim.render(&format!("missing {}", missing.join(", ")))),
    )
}

/// Renders a variable table: name, value (masked when secret) and source,
/// with hints for missing ones.
pub fn variables(t: &Theme, vars: &[VarInfo], hints: bool) -> String {
    let name_w = vars.iter().map(|v| v.name.len() + 2).max().unwrap_or(0);
    let mut b = String::new();
    for v in vars {
        let pad = " ".repeat(name_w - v.name.len() - 2);
        if v.missing && v.ref_runs && hints {
            let _ = writeln!(
                b,
                "  {}{}  missing  captured by {}, which # @ref runs first",
                t.warn.render(&format!("○ {}", v.name)),
                pad,
                v.captured_by
            );
        } else if v.missing && !v.captured_by.is_empty() && hints {
            let _ = writeln!(
                b,
                "  {}{}  missing  captured by {} — run `apic run {}` first",
                t.fail.render(&format!("✗ {}", v.name)),
                pad,
                v.captured_by,
                v.captured_by
            );
        } else if v.missing && hints {
            let _ = writeln!(
                b,
                "  {}{}  missing  pass --var {}=...",
                t.fail.render(&format!("✗ {}", v.name)),
                pad,
                v.name
            );
        } else if v.missing {
            let _ = writeln!(
                b,
                "  {}{}  missing",
                t.fail.render(&format!("✗ {}", v.name)),
                pad
            );
        } else {
            let _ = writeln!(
                b,
                "  {}{}  {}  {}",
                t.ok.render(&format!("✓ {}", v.name)),
                pad,
                mask(v),
                t.dim.render(&v.source)
            );
        }
    }
    b
}

/// Returns a variable's value, or *** when it is a secret.
pub fn mask(v: &VarInfo) -> String {
    if v.secret {
        return MASKED.to_string();
    }
    truncate(&v.value, 60)
}

/// Renders a bold section title preceded by a blank line.
pub fn section(t: &Theme, title: &str) -> String {
    format!("\n{}\n", t.bold.render(title))
}

/// Prefixes every line with two spaces.
pub fn indent(s: &str) -> String {
    format!("  {}", s.trim_end_matches('\n').replace('\n', "\n  "))
}

/// Flattens newlines and cuts `s` to `n` characters, appending an
/// ellipsis.
pub fn truncate(s: &str, n: usize) -> String {
    let s = s.replace('\n', " ");
    match s.char_indices().nth(n) {
        None => s,
        Some((at, _)) => format!("{}…", &s[..at]),
    }
}

/// Formats a byte count.
pub fn size(n: usize) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / 1024.0 / 1024.0)
    }
}
