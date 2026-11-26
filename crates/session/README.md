# apic-session

The state apic keeps between runs under `.apic/`: captured values
(`session.json`) and the cookie jar (`cookies.json`). Files are written
0600 in a 0700 directory with a `.gitignore`, in a fixed form:
two-space indentation, sorted keys, `<>&` as `<` and so on, a trailing
newline.

## Session

| Item | What it does |
| --- | --- |
| `Store::open(root) -> Result<Store, Error>` / `open(root)` | Loads `.apic/session.json`, or an empty store |
| `Store::new_memory()` / `new_memory()` | Never written to disk |
| `snapshot()` | A memory-only copy |
| `envs: BTreeMap<String, BTreeMap<String, String>>` | The data; `""` is stored under `DEFAULT_ENV` (`"default"`) |
| `vars(env)`, `get(env, name) -> Option<&str>` | Read |
| `set(env, pairs)`, `clear(env)` (`"*"` clears all) | Change, in memory |
| `env_names()` | Environments with values, sorted |
| `save()` | Writes the file (no-op for a memory store) |
| `ensure_dir(dir)` | Creates a `.apic` directory and its `.gitignore`; the history uses it too |
| `DIR`, `FILE`, `COOKIE_FILE`, `DEFAULT_ENV` | `.apic`, `session.json`, `cookies.json`, `default` |
| `Error(String)` | The message (`open /p/.apic/session.json: permission denied`) |

Writes are plain create-truncate-write, as they always were: no lock file
and no rename.

## Cookies

The jar is RFC 6265 without a public-suffix list, implemented in
`src/cookiejar.rs` (domain and path matching, host-only cookies, Max-Age
and Expires, Secure with the localhost exception, IDN hosts, the
last-two-labels storage key). It takes and gives plain data:

```rust
pub struct RequestUrl { pub scheme: String, pub host: String, pub path: String }
// host is "host" or "host:port" ("[::1]:8080"); path has its escapes resolved.
// RequestUrl::parse("https://h:1/p?q") is a convenience; hostname() drops the port.

pub struct SetCookie { name, value, quoted, path, domain,
                       expires: Option<Time>, max_age: i64, secure, http_only }
// max_age: 0 absent, negative "delete now", else seconds.
```

| Item | What it does |
| --- | --- |
| `Jar::open(root)` / `open_jar(root)`, `Jar::new_memory()` / `new_memory_jar()` | The project's jar. `Jar` is a cheap `Clone` handle (`Send + Sync`) |
| `jar.http(env) -> EnvJar` | The cookies of one environment, for the HTTP client |
| `EnvJar::cookie_header(&url, existing: Option<&str>) -> Option<String>` | The `Cookie` header value to send: jar cookies appended to what the request already carries; `None` when the jar adds nothing. Call it before the request and before each redirect |
| `EnvJar::set_cookie_headers(&url, &[lines])` | Give it every `Set-Cookie` value of each response (redirects included), one line per entry |
| `EnvJar::cookies(&url) -> Vec<RequestCookie>`, `EnvJar::set_cookies(&url, &[SetCookie])` | The same in parsed form |
| `parse_set_cookie(line) -> Option<SetCookie>`, `read_set_cookies(&[lines])` | The `Set-Cookie` parser on its own |
| `jar.cookies(env) -> Vec<Cookie>` | Stored, unexpired cookies sorted by domain, path, name |
| `Cookie { name, value, domain, path, expires: Option<Time>, secure, http_only, host_only }` | `expiry_text(now)`: `until cleared`, `expires in 1h0m0s`, `expired` |
| `jar.env_names()`, `jar.clear(env)`, `jar.save()` | As for the session; `save` does nothing unless something changed, and removes the file when no cookie is left |
| `jar.set_now(f)` | Replaces the clock (tests) |
| `Time` | `apic_gocompat::Time`, re-exported |

## Notes

- One clock (`set_now`) decides both what is sent and what is stored.
- `cookies.json` lists an environment's cookies in the order they were
  first set.
- A new `expires` is written in UTC (`...Z`). A time with a local offset
  is read too, and a time read from the file is written
  back with the offset it had.
- `testdata/cookie_reference.json` holds jar scenarios with their
  expected outcomes; `cookies_tests.rs` replays them.
