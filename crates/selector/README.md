# apic-selector

The selector language of `# @capture` and `# @assert`: `status`,
`statusText`, `duration`, `header.<name>` (`.#`, `[n]`), `cookie.<name>`,
`body`, `body.$` and `body.$.<path>` (JSONPath: keys, indexes, slices,
wildcards, `..`, filters, `.#` and `.length`).

## Input: a plain response

The crate depends on no HTTP crate. Whoever sent the request fills in:

```rust
pub struct Response {
    pub status: i64,            // 200
    pub status_text: String,    // "OK": the reason phrase alone
    pub headers: Headers,       // in arrival order, repeats kept
    pub body: Vec<u8>,          // after content decoding
    pub duration: Duration,     // `duration` selects its whole milliseconds
}
pub struct Headers(pub Vec<(String, String)>);
```

`Headers` has `new()`, `add(name, value)`, `values(name)`, `get(name)` and
`FromIterator<(name, value)>`; names compare without regard to ASCII case.
`cookie.<name>` reads the `Set-Cookie` values itself, so every `Set-Cookie`
line must be its own entry (never joined with commas).

## API

| Item | What it does |
| --- | --- |
| `select(&Response, sel) -> Result<Option<String>, Error>` | The selected text; `Ok(None)` is "valid, but nothing there" |
| `select_value(&Response, sel) -> Result<Option<Value>, Error>` | The same with its JSON type: `Value { text, kind }` |
| `check(sel) -> Result<(), Error>` | Parses a selector without a response, for validation |
| `Kind` | `String`, `Number`, `Bool`, `Null`, `Array`, `Object`; `as_str()` / `Display` give `string`, `number`, `boolean`, `null`, `array`, `object` |
| `Error` | `Unknown { selector }` (none of the known forms; `is_unknown()`) or `Invalid(message)`; `Display` is the message |
| `leading(s) -> (&str, &str)` | Splits `body.$.items[?(@.a == 1)].id == 2` into the selector and the rest |
| `parse_path(tail) -> Result<Path, String>` | Parses the part after `body.$` |
| `Number` | An exact number: `Number::parse(s)`, `Ord`, `is_int()`. Decimal with exponent, `0x`/`0o`/`0b`, `p` exponents, `_` separators; no fractions |
| `compile_regex(pattern) -> Result<Regex, String>` | The `regex` crate with RE2's reading: ASCII `\d \w \s \b`, `\Q..\E`, literal `[`, `&`, `~` in classes. Use it wherever a user's pattern is compiled |

## Notes

- A selected object or array keeps the server's text byte for byte; a set of
  matches is `[a,b,c]` of the raw values. `body.$` on an object or array
  includes any white space after it.
- The JSON reader is the crate's own (`src/json.rs`); the `gjson` crate is
  not used.
- A body that is not valid UTF-8 is read with U+FFFD in place of the bad
  bytes.
- The reason a regex does not compile is worded by the `regex` crate
  (`error parsing regexp: <reason>: `<pattern>``).
- `testdata/reference.json` holds about 1,500 selections recorded from the
  original implementation; `tests/reference.rs` replays them.
