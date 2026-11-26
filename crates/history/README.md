# apic-history

The last few responses of each named request, per environment, under
`.apic/history/<env>/<request>/`, one JSON file per entry (0600, two-space
indentation, trailing newline, named by UTC time:
`20261006T090000.000000000Z.json`, with `-2`, `-3`, ... for entries of the
same instant).

## API

| Item | What it does |
| --- | --- |
| `Store::new(root: &Path, keep: i64)` | The project's history; nothing is written until `record`. `keep()` returns the limit |
| `record(env, name, at: Time, result: &str) -> Result<(), Error>` | Stores the JSON of a run's result as the newest entry and prunes beyond `keep`; `keep <= 0` records nothing |
| `list(env, name) -> Result<Vec<Entry>, Error>` | Newest first, without results |
| `get(env, name, index: i64) -> Result<Entry, Error>` | Entry `index` (1 is the newest) with its result; out of range is `Error::Range` |
| `requests(env) -> Result<Vec<Recorded>, Error>` | Requests with history, by name. `Recorded { request, entries }` (`Serialize`) |
| `clear(env, name) -> Result<usize, Error>` | Empty name: the whole environment; env `"*"`: everything. Returns the entries removed |
| `Entry { index, time: Time, ok, status, status_text, duration_ms, size, file, result: Option<String> }` | `Serialize` in that order, `time` as RFC 3339, `status_text` left out when empty, `result` never. `file` is relative to the project root with `/` |
| `Error` | `Range(RangeError { name, index, have })` or `Other(String)`; `Display` is the message (`no history for login`) |
| `compare(from: &str, to: &str) -> Result<Vec<Change>, String>` | What changed between two recorded results: status, then body (JSON by structure, text by line, binary as one change) |
| `Change { path, op, from: Option<String>, to: Option<String> }` | `op` is `ADDED`, `REMOVED` or `CHANGED`; `from`/`to` are JSON text and serialise raw (`"from":1.50`) |
| `DIR`, `DEFAULT_ENV` | `history`, `default` |
| `Time` | `apic_gocompat::Time`, re-exported; `Time::now()` for `record` |

## Notes

- A result is stored compacted then re-indented, with `<>&` escaped, exactly
  as before. An entry file that is not a whole entry is passed over.
- Numbers are compared as written: `1.50` and `1.5` differ.
- `serde_json`'s `raw_value` feature is enabled by this crate.
