# apic-report

The self-contained HTML report of an `apic run` or an `apic test`
(`apic_report`): one file, inline CSS, no JavaScript, light and dark. It is
what `apic run --report file.html` and `apic test --format html` write.

```rust
use apic_report::Meta;

let meta = Meta {
    version: version.into(),
    env: runner.opts.env.clone(),
    time: Some(started),            // None means now
    redacted: redact,
    project: root.display().to_string(),
    ..Meta::default()               // title: "apic run" / "apic test" when empty
};
apic_report::run(&mut file, &meta, &results)?;         // &[apic_runner::Result]
apic_report::features(&mut file, &meta, &cucumber)?;   // the cucumber JSON of `apic test`
```

## API

| Item | What it does |
| --- | --- |
| `Meta { title, version, env, time: Option<Time>, redacted, project }` | What the page says about the run as a whole. The time is shown in UTC, to the second |
| `run(w: &mut dyn Write, &Meta, &[Result]) -> Result<(), Error>` | One section per request, `# @ref` dependencies nested under the request that ran them, failures open. Values are the display ones: sensitive headers masked, everything masked for a result with `redact` |
| `features(w, &Meta, cucumber: &[u8]) -> Result<(), Error>` | One section per feature and scenario. A background's steps are shown under the scenario they ran for. Empty input is a report of nothing, `all passed`. The JSON is already masked under --redact |
| `Error` | `Parse(String)` (`parse cucumber report: ...`), `Template(String)`, `Io(std::io::Error)` |

The output is deterministic for a given input, so it is golden-tested.

## The page

`report.html.tmpl` is the page, compiled in with `include_str!` (CRLF
folded to LF). `src/template.rs` is the engine for the notation it is
written in:

- `{{define "name"}}...{{end}}` blocks, and `{{template "name" .}}`.
- `{{.Field}}`, `{{.A.B}}`, `{{.}}`; `{{.Time.Format "2006-01-02 15:04:05
  UTC"}}`.
- `{{if c}}`, `{{else if c}}`, `{{else}}`, `{{range list}}`, `{{end}}`.
  Empty, zero, false and absent are false.
- Functions: `not`, `or`, `lt`, `gt`, `eq`, `ne`, `len`, `ms` (a duration
  as `12 ms`) and `json` (indents a JSON text, leaves anything else).
- Everything printed is escaped: `&`, `<`, `>`, `"` (`&#34;`), `'`
  (`&#39;`) and `+` (`&#43;`). The escaping is the same in an element, a
  quoted attribute and the title, which is all the page uses; do not put a
  value inside `<style>`, a script, a URL attribute or an unquoted
  attribute.

Pipelines, variables, `with` and trim markers are not there: the page does
not use them. A new field for the page goes into the record `view` (or
`features`) builds in `src/html.rs`, under the name the template reads.

## Tests

`testdata/*.golden.html` are compared after the server's port, the dates
and the measured times are normalised; a mismatch leaves `name.got` beside
the golden. `testdata/run-wide*.golden.json` hold a flow with every kind of
result (retried, skipped, binary, unanswered, with dependencies), as `apic
run --json` printed it, and the page it gives, with and without --redact.
`UPDATE_GOLDEN=1 cargo test -p apic-report` accepts new output.
