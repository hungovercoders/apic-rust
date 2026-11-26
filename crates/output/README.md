# apic-output

What apic prints (`apic_output`): the renderers that turn a run's results
into text for people, the writers the CLI calls, the shared theme, and
underneath them `apic_output::style`, apic's own layer for styled terminal
text. The terminal UI and the screenshot generator are built on the same
renderers and the same `style` module, so all three draw the same thing.

```rust
use apic_output::{style, Theme};

let t = Theme::default();
print!("{}", apic_output::result(&t, &res, apic_output::Options::default()));
apic_output::human(&mut std::io::stdout(), &res, verbose);   // the same, deps first
apic_output::json(&mut std::io::stdout(), &res)?;            // one JSON line per request sent

style::set_color_profile(style::Profile::Ascii);             // --no-color, NO_COLOR, --json, not a terminal
let line = style::truncate(&t.dim.render("a long line"), width, "…");
```

## Conventions

- Results are borrowed: `&apic_runner::Result`, and a flow is
  `&[apic_runner::Result]`. Nothing is kept.
- A renderer returns a `String` and takes the theme first: `(t: &Theme,
  ...)`. A writer takes `w: &mut dyn std::io::Write` first and ignores write
  errors, except `json` and `json_nested`, which return `io::Result<()>`.
- `Theme::default()` is cheap; the writers build their own. Pass one by
  reference to the renderers.
- Two functions are called `truncate`: `apic_output::truncate(s, n)` cuts
  at `n` characters and flattens newlines (values in reports);
  `style::truncate(s, cells, tail)` cuts at a width in terminal cells and
  keeps escape sequences (lines on screen).

## Theme

`Theme { bold, dim, ok, warn, fail, url, header, capture, accent, key, str,
num, lit }`, each a `style::Style`; `key`, `str`, `num` and `lit` are the
JSON highlighting. Methods: `method(m)` (an HTTP method in its colour),
`status(code, text)` (`200 OK` by class), `status_code(code)`,
`latency(ms)` (green under 200, amber under 1000, else red), `mark(pass)`
(`✓` or `✗`). Status codes and latencies are `i64`, as in
`apic_runner::Response`.

## Renderers

| Function | Renders |
| --- | --- |
| `result(t, res, Options { verbose, width })` | The whole report of one request, as `apic run` prints it; `width` 0 means 80 |
| `request_line(t, res)` | `METHOD url`, the URL masked when redacting |
| `request_detail(t, res)` | Request headers, tls, proxy, auth note and body |
| `status_line(t, res)` | `200 OK · 87 ms · 412 B`, `skipped (...)`, or the errors of a request that got no response |
| `timings(t, res)` | `dns 12 ms · connect ... · new connection`, empty without a breakdown |
| `attempt(t, &Progress)` | `attempt 2/5 · why` for a retried request |
| `response_headers(t, res)` | Sorted, lower-cased, masked; empty without a response |
| `render_body(t, &[u8])` | A JSON body indented and highlighted; anything else as it is |
| `body_or_summary(t, res, &[u8])` | The body, or one line for a binary one |
| `checks(t, res, width, expected)` | Assertions, captures, errors and warnings |
| `summary_table(t, &[Result])`, `summary_line(t, &[Result])` | One row per request and the totals; `3 passed, 1 skipped` |
| `describe(t, &Description, &[apic_httpfile::Header])` | What `apic describe` shows |
| `readiness(t, &Description)` | The boxed verdict that ends a description |
| `variables(t, &[VarInfo], hints)`, `mask(&VarInfo)` | The variable table; a value, or `***` for a secret |
| `section(t, title)`, `indent(s)`, `truncate(s, n)`, `size(n)` | The small pieces: a bold title after a blank line, two-space indent, `…` cut, `1.5 KB` |
| `highlight_json(t, s)` | Colours an already indented JSON document, line by line |
| `history_time(Time)`, `history_entries(t, &[Entry])`, `history_entry(&Entry)`, `changes(t, &[Change], width)` | The response history and the differences between two responses; times are local |

Writers: `json(w, res)` (dependencies first, each a line of its own, none
nested), `json_nested(w, res)` (one line, dependencies under `ran_first`),
`body(w, res)`, `human(w, res, verbose)`, `deps(w, res, verbose)`,
`summary(w, results)`. A JSON line is `Result::to_json()` and a newline.

## style

Everything is in `apic_output::style`.

### Colour profile

`Profile` is `TrueColor`, `Ansi256`, `Ansi` or `Ascii`. Every render goes
through the profile in force:

| Function | What it does |
| --- | --- |
| `color_profile()` | The profile in force: a thread override, else the process-wide one, else `env_color_profile()`, worked out once on first use |
| `set_color_profile(p)` | Sets it for the process |
| `with_color_profile(p, \|\| ...)` | Sets it for this thread while the closure runs. Tests use this, never `set_color_profile`: they run side by side |
| `env_color_profile()` | From the environment, for stdout: `Ascii` when `NO_COLOR` is set, `CLICOLOR=0`, `CI` is set or stdout is not a terminal; else by `COLORTERM` (`truecolor`, `24bit`, `yes`, `true`) and `TERM` (`xterm-kitty` and friends, `*256color`, `*color`, `*ansi`, `xterm`, `linux`). `CLICOLOR_FORCE` gives `Ansi` where there would be none. On Windows a terminal is `TrueColor` |
| `detect_color_profile(is_terminal, &getenv)` | The same with its inputs passed in |
| `has_dark_background()`, `set_has_dark_background(bool)` | Which side of an adaptive colour is used. Dark unless set: the terminal is never asked, at start-up or later |

Under `Ascii` no escape sequence is written; padding, borders, alignment
and wrapping still apply.

### Style

`Style::new()` and setters that each return a new style; `render(&str)`
uses the profile in force and `render_with(profile, &str)` a given one.

- Attributes: `bold`, `italic`, `underline`, `strikethrough`, `reverse`,
  `blink`, `faint` (each takes a `bool`), `underline_spaces`,
  `strikethrough_spaces`.
- Colours: `foreground(c)`, `background(c)`, `color_whitespace(bool)`,
  `margin_background(c)`, `border_foreground(c)`, `border_background(c)`,
  and `border_foreground_sides(&[Color])`, `border_background_sides`. A
  colour is `impl Into<Color>`: `"212"`, `"#ff87d7"`, `"#f8d"`, a `u8`, or
  `Color::adaptive(light, dark)`, `Color::complete(truecolor, ansi256,
  ansi)`, `Color::None`.
- Size: `width(n)` (wraps and pads), `height(n)`, `max_width(n)`,
  `max_height(n)`, `inline(bool)`, `tab_width(n)` (4; 0 drops tabs, -1 keeps
  them).
- Position: `align_horizontal(p)`, `align_vertical(p)`, `align(&[p])`, with
  `Position::LEFT`, `CENTER`, `RIGHT`, `TOP`, `BOTTOM` or `Position(0.25)`.
- Box: `padding(&[..])` and `margin(&[..])` take one to four values in the
  CSS order (all; vertical, horizontal; top, horizontal, bottom; top, right,
  bottom, left), with `padding_top` ... `margin_left` for one side.
  `border(b, &[bool])` sets the border and its sides (`&[]` is all four);
  `border_style(b)` only the characters; `border_top(bool)` ... one side.
  Borders: `normal_border()`, `rounded_border()`, `thick_border()`,
  `double_border()`, `hidden_border()`, `block_border()`,
  `outer_half_block_border()`, `inner_half_block_border()`,
  `markdown_border()`, `ascii_border()`, or a `Border { top, bottom, left,
  right, top_left, ... }` of your own.
- `horizontal_frame_size()`, `vertical_frame_size()`: the room margins,
  border and padding take.

Sizes are `i32`; zero means none.

The sequences are a contract, because the UI's tests and the screenshots
compare rendered strings: `\x1b[1;38;5;212mtext\x1b[0m`, one opening and
one reset per line, attributes in the order bold, italic, underline,
reverse, blink, faint, then foreground, background, underline once more and
strikethrough. Basic colours are `30`-`37` and `90`-`97` (`40`-`47`,
`100`-`107` behind), the 256 are `38;5;n` (`48;5;n`), 24-bit is
`38;2;r;g;b`. A colour the profile cannot show is brought down: hex to the
nearest of the 256, those to the nearest of the 16.

### Layout and measuring

| Function | What it does |
| --- | --- |
| `width(s)`, `height(s)`, `size(s)` | The widest line in cells and the number of lines of a block (`usize`) |
| `join_horizontal(pos, &[&str])`, `join_vertical(pos, &[&str])` | Blocks side by side or one above the other, padded |
| `place(w, h, hpos, vpos, s)`, `place_horizontal(w, pos, s)`, `place_vertical(h, pos, s)` | A block in a larger box of spaces |
| `string_width(s)` | Cells of one line, escape sequences not counted |
| `strip(s)` | The text without its escape sequences |
| `truncate(s, cells, tail)`, `truncate_left(s, cells, prefix)`, `cut(s, left, right)` | Cut by cells; sequences are kept, those after the cut too |
| `hardwrap(s, limit, preserve_space)` | Break every `limit` cells |
| `wordwrap(s, limit, breakpoints)` | Break at spaces, hyphens and `breakpoints`; long words stay whole |
| `wrap(s, limit, breakpoints)` | The same, and long words are broken |
| `first_grapheme_cluster(s)` | The first cluster and its width |

Limits are `i32` and may be negative (nothing fits, or no wrapping), so
`width - used` can be passed as it is; measurements are `usize`. Widths
count grapheme clusters: a letter with its marks is one cell, CJK and emoji
two, a flag or a joined emoji two.

## Where it differs from before

- A body that is not valid UTF-8 is shown with U+FFFD by the string
  renderers; `body(w, res)` still writes the bytes.
- Text is segmented by apic's own rules, the common ones of the Unicode
  annex. Rare characters can measure a cell differently (a hundred or so
  code points out of sixteen thousand sampled, none in what apic prints).
- A style with a width that wraps text already carrying sequences switches
  the style off and on again around the break with the usual parameters,
  not with every variant of underline or an underline colour.
- `place*` pads with spaces only, and a style has no value of its own, no
  transform and no inheritance: nothing used them.
- On Windows a terminal is taken to show 24-bit colour, whatever its
  build.

## Tests

`testdata/style_golden.json` holds about 600 styles with their output under
each profile, and 2,600 calls of the text and layout functions, recorded
from the libraries apic used to draw with; `src/style/tests.rs` replays
them byte for byte. `testdata/golden.go` rewrites the outputs from the
cases in the file (it needs a Go module with lipgloss v1.1.0 and x/ansi
v0.11.9). `testdata/render_golden.json` holds results as `apic run --json`
printed them, descriptions, history entries and what every renderer made
of them under three profiles; `src/output_tests.rs` replays it. After a
deliberate change to a renderer, `UPDATE_GOLDEN=1 cargo test -p
apic-output` rewrites the renders in that file.
