//! The cards are HTML in the docs theme, rendered by headless Chromium at
//! the canvas size; the thumbnail is the same at YouTube's 1280x720.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use regex::Regex;

use crate::episode::{Card, Episode};
use crate::video::BG;

const LOGO_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" role="img" aria-label="apic"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#7c5cff"/><stop offset="1" stop-color="#ff5ca8"/></linearGradient></defs><rect width="64" height="64" rx="14" fill="url(#g)"/><path d="M14 22 L24 32 L14 42" fill="none" stroke="#fff" stroke-width="6" stroke-linecap="round" stroke-linejoin="round"/><path d="M30 42 L38 22 L46 42" fill="none" stroke="#fff" stroke-width="6" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;

/// The head of a card's page up to the end of its style sheet, with
/// `{bg}` where the background colour goes.
const CARD_HEAD: &str = r##"<!doctype html><html><head><meta charset="utf-8"><style>
html,body{margin:0;width:1920px;height:1080px;background:{bg};color:#cdd6f4;font-family:Inter,sans-serif;overflow:hidden}
.wrap{position:absolute;inset:0;display:flex;flex-direction:column;justify-content:center;padding:0 160px 90px;box-sizing:border-box}
.series{font-size:36px;color:#c9bbff;letter-spacing:.02em}
h1{font-size:88px;line-height:1.1;margin:20px 0 28px;font-weight:700;background:linear-gradient(100deg,#7c5cff,#ff5ca8);-webkit-background-clip:text;color:transparent}
h2{font-size:64px;line-height:1.15;margin:0 0 36px;font-weight:700;color:#fff}
.logo{position:absolute;top:72px;right:160px;width:140px;height:140px}
.logo svg{width:140px;height:140px}
code,pre{font-family:"DejaVu Sans Mono",monospace}
pre{font-size:32px;line-height:1.5;margin:0;color:#cdd6f4;white-space:pre-wrap}
pre .hl,td .hl{color:#a6e3a1}
.cmds{display:grid;grid-template-columns:1fr 1fr;gap:28px 48px}
.cmd{background:#181825;border:1px solid #313244;border-radius:16px;padding:26px 32px}
.cmd .os{font-size:26px;color:#c9bbff;margin-bottom:12px}
.cmd pre{font-size:28px;color:#a6e3a1;word-break:break-all}
table{border-collapse:collapse;font-size:30px;width:100%}
td{padding:10px 24px 10px 0;vertical-align:top;border-bottom:1px solid #313244}
td:first-child{white-space:nowrap;color:#a6e3a1;font-family:"DejaVu Sans Mono",monospace;font-size:28px}
.dim{color:#a6adc8}
.foot{position:absolute;left:160px;bottom:120px;font-size:30px;color:#a6adc8}
.foot .hl{color:#7c5cff;font-family:Inter,sans-serif}
"##;

/// The head of the thumbnail's page, likewise.
const THUMB_HEAD: &str = r##"<!doctype html><html><head><meta charset="utf-8"><style>
html,body{margin:0;width:1280px;height:720px;background:{bg};overflow:hidden;font-family:Inter,sans-serif;color:#fff}
.l{position:absolute;left:64px;top:64px;width:600px}
.logo svg{width:120px;height:120px}
.n{font-size:150px;font-weight:800;line-height:1;margin-top:24px;background:linear-gradient(100deg,#7c5cff,#ff5ca8);-webkit-background-clip:text;color:transparent}
.t{font-size:64px;font-weight:700;line-height:1.1;margin-top:16px}
.s{font-size:30px;color:#c9bbff;margin-top:22px}
.shot{position:absolute;left:660px;top:90px;width:900px;border-radius:18px;overflow:hidden;box-shadow:0 30px 80px rgba(0,0,0,.6);border:2px solid #45475a;transform:rotate(-3deg)}
.shot img{display:block;width:900px}
"##;

static RE_TICK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("`([^`]+)`").expect("a valid pattern"));

/// Escapes text for the body of an element or a quoted attribute.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&#34;"),
            '\'' => out.push_str("&#39;"),
            '\0' => out.push('\u{fffd}'),
            c => out.push(c),
        }
    }
    out
}

/// Escapes a value the page shows as it is. A plus sign goes too, so the
/// text cannot be read as UTF-7 by a browser that guesses the encoding.
fn escape_value(s: &str) -> String {
    escape(s).replace('+', "&#43;")
}

/// Escapes text, then turns backtick spans into highlighted spans and dims
/// a leading "$ " prompt on each line.
fn highlight(s: &str) -> String {
    let esc = escape(s);
    let esc = RE_TICK.replace_all(&esc, r#"<code class="hl">$1</code>"#);
    let lines: Vec<String> = esc
        .split('\n')
        .map(|line| match line.strip_prefix("$ ") {
            Some(rest) => format!(r#"<span class="dim">$</span> {rest}"#),
            None => line.to_string(),
        })
        .collect();
    lines.join("\n")
}

/// Escapes text and turns its newlines into line breaks.
fn lines(s: &str) -> String {
    escape(s).replace('\n', "<br>")
}

/// Makes a path safe as the address of an image: a value that could be
/// read as a scheme other than http, https or mailto is replaced, and
/// every byte that is not plain in a URL is percent-encoded.
fn url_attr(s: &str) -> String {
    if let Some((scheme, _)) = s.split_once(':') {
        let known = ["http", "https", "mailto"]
            .iter()
            .any(|k| scheme.eq_ignore_ascii_case(k));
        if !scheme.contains('/') && !known {
            return "#ZgotmplZ".to_string();
        }
    }
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in bytes.iter().enumerate() {
        let plain = match c {
            b'!' | b'#' | b'$' | b'&' | b'*' | b'+' | b',' | b'/' | b':' | b';' | b'=' | b'?'
            | b'@' | b'[' | b']' => true,
            b'-' | b'.' | b'_' | b'~' => true,
            // A valid escape is not encoded again.
            b'%' => {
                i + 2 < bytes.len()
                    && bytes[i + 1].is_ascii_hexdigit()
                    && bytes[i + 2].is_ascii_hexdigit()
            }
            c => c.is_ascii_alphanumeric(),
        };
        if plain {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{c:02x}"));
        }
    }
    escape_value(&out)
}

/// A card's page.
fn card_html(c: &Card) -> String {
    let mut b = CARD_HEAD.replace("{bg}", BG);
    b.push_str(&format!(
        "</style></head><body><div class=\"logo\">{LOGO_SVG}</div><div class=\"wrap\">\n"
    ));
    if !c.series.is_empty() {
        b.push_str(&format!(
            "<div class=\"series\">{}</div>",
            escape_value(&c.series)
        ));
    }
    b.push('\n');
    if !c.title.is_empty() {
        b.push_str(&format!("<h1>{}</h1>", lines(&c.title)));
    }
    b.push('\n');
    if !c.heading.is_empty() {
        b.push_str(&format!("<h2>{}</h2>", escape_value(&c.heading)));
    }
    b.push('\n');
    if !c.commands.is_empty() {
        b.push_str("<div class=\"cmds\">");
        for cmd in &c.commands {
            b.push_str(&format!(
                "<div class=\"cmd\"><div class=\"os\">{}</div><pre>{}</pre></div>",
                escape_value(&cmd.label),
                escape_value(&cmd.command)
            ));
        }
        b.push_str("</div>");
    }
    b.push('\n');
    if !c.table.is_empty() {
        b.push_str("<table>");
        for row in &c.table {
            b.push_str("<tr>");
            for cell in row {
                b.push_str(&format!("<td>{}</td>", highlight(cell)));
            }
            b.push_str("</tr>");
        }
        b.push_str("</table>");
    }
    b.push('\n');
    if !c.text.is_empty() {
        b.push_str(&format!("<pre>{}</pre>", highlight(&c.text)));
    }
    b.push('\n');
    if !c.foot.is_empty() {
        b.push_str(&format!("<div class=\"foot\">{}</div>", highlight(&c.foot)));
    }
    b.push_str("\n</div></body></html>");
    b
}

/// The thumbnail's page; `shot` is the PNG it frames.
fn thumb_html(ep: &Episode, shot: &Path) -> String {
    let mut b = THUMB_HEAD.replace("{bg}", BG);
    b.push_str(&format!(
        "</style></head><body>\n<div class=\"l\"><div class=\"logo\">{LOGO_SVG}</div><div class=\"n\">#{}</div><div class=\"t\">{}</div><div class=\"s\">From zero to apic</div></div>\n<div class=\"shot\"><img src=\"{}\"></div>\n</body></html>",
        ep.lesson,
        lines(&ep.thumbnail.words),
        url_attr(&shot.to_string_lossy())
    ));
    b
}

/// Writes a card's HTML next to the PNG it becomes.
pub(crate) fn render_card(c: &Card, html: &Path) -> Result<(), String> {
    std::fs::write(html, card_html(c)).map_err(|err| err.to_string())
}

/// Writes the thumbnail's HTML; `shot` is the PNG it frames.
pub(crate) fn render_thumb(ep: &Episode, shot: &Path, html: &Path) -> Result<(), String> {
    std::fs::write(html, thumb_html(ep, shot)).map_err(|err| err.to_string())
}

/// The browser the cards are rendered with: $CHROMIUM, or the first of the
/// usual names on PATH.
pub(crate) fn find_chromium() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("CHROMIUM").filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    ["chromium", "chromium-browser", "google-chrome", "chrome"]
        .iter()
        .find_map(|name| record::look_path(name))
        .ok_or_else(|| "no chromium, chromium-browser or google-chrome on PATH; set CHROMIUM to the browser to render the cards with".to_string())
}

/// Reports whether this process runs as root.
fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "0")
}

/// Renders an HTML file to a PNG of the given size. Chromium refuses to
/// run as root without --no-sandbox, which a container's build usually is.
pub(crate) fn screenshot(
    chromium: &Path,
    html: &Path,
    png: &Path,
    w: u32,
    h: u32,
) -> Result<(), String> {
    let abs = std::path::absolute(html).map_err(|err| err.to_string())?;
    let mut cmd = Command::new(chromium);
    if is_root() {
        cmd.arg("--no-sandbox");
    }
    cmd.args(["--headless=new", "--disable-gpu", "--hide-scrollbars"])
        .arg(format!("--window-size={w},{h}"))
        .arg(format!("--screenshot={}", png.display()))
        .arg(format!("file://{}", abs.display()));
    let (out, result) = record::combined_output(&mut cmd);
    if let Err(err) = result {
        return Err(format!(
            "chromium: {err}\n{}",
            String::from_utf8_lossy(&out)
        ));
    }
    if !png.exists() {
        return Err(format!("chromium wrote nothing for {}", html.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::episode::Thumbnail;

    /// A card renders to HTML with its text escaped and its backticks
    /// turned into highlights, and nothing else.
    #[test]
    fn render_card_escapes_and_highlights() {
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("c.html");
        let card = Card {
            heading: "h <b>".to_string(),
            text: "$ `apic run` <x>".to_string(),
            foot: "see `apic.sh`".to_string(),
            ..Card::default()
        };
        render_card(&card, &html).unwrap();
        let got = std::fs::read_to_string(&html).unwrap();
        for want in [
            "h &lt;b&gt;",
            r#"<span class="dim">$</span> <code class="hl">apic run</code> &lt;x&gt;"#,
            r#"<code class="hl">apic.sh</code>"#,
        ] {
            assert!(got.contains(want), "rendered card lacks {want:?}");
        }
        assert!(got.contains(&format!("background:{BG};")));
        assert!(got.ends_with("</div>\n</div></body></html>"), "{got}");
    }

    /// The thumbnail carries the lesson's number, its words over two lines
    /// and the frame, whose path cannot break out of its attribute.
    #[test]
    fn thumbnail_page() {
        let ep = Episode {
            lesson: 4,
            thumbnail: Thumbnail {
                words: "One &\ntwo".to_string(),
                clip: "a".to_string(),
            },
            ..Episode::default()
        };
        let got = thumb_html(&ep, Path::new("/tmp/a b/shot\".png"));
        assert!(got.contains(r#"<div class="n">#4</div><div class="t">One &amp;<br>two</div>"#));
        assert!(
            got.contains(r#"<img src="/tmp/a%20b/shot%22.png">"#),
            "{got}"
        );
        assert_eq!(url_attr("javascript:alert(1)"), "#ZgotmplZ");
        assert_eq!(url_attr("a/b:c?x=1&y=2+3"), "a/b:c?x=1&amp;y=2&#43;3");
        assert_eq!(escape_value("a+b 'c'"), "a&#43;b &#39;c&#39;");
    }
}
