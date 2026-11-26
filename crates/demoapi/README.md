# apic-demoapi

Backs `apic demo` (`apic_demoapi`): a tiny, fully in-memory HTTP API and
the example `.http` project that goes with it, embedded from `project/` at
build time (`build.rs` lists the files), so apic can be tried with no
network access and no git clone.

```rust
let listener = apic_httpd::Listener::bind("127.0.0.1:8089")?;
let (written, skipped) = apic_demoapi::write_project("apic-demo", 8089, false)?;
apic_httpd::serve(listener, apic_demoapi::new(), &cancel);
```

## API

| Item | Signature / notes |
| --- | --- |
| `new()` | `-> apic_httpd::Handler`: the API, with todos and jobs of its own; each call is a fresh instance |
| `write_project(dir, port, force)` | `-> Result<(Vec<String>, Vec<String>), String>`: writes the bundled project into `dir` (created if needed) plus an `http-client.env.json` pointing at `http://localhost:<port>`, and returns the paths written and the paths skipped. Existing files are left alone unless `force`. `http-client.private.env.json` is created `0600` |
| `set_version(v)` / `version()` | What `GET /health` reports; `dev` until set |

## Routes

Bearer means `Authorization: Bearer <anything>`, as `login` returns.

| Route | Notes |
| --- | --- |
| `POST /auth/login` | `{"user","password"}` (alice / s3cret) -> `{"access_token"}` |
| `GET /me` | bearer |
| `GET /basic-auth/{user}/{password}` | HTTP basic auth |
| `POST /oauth/token` | client-credentials grant (demo-client / demo-secret) |
| `GET /keyed` | `X-Api-Key: demo-key` |
| `GET /todos?done=&page=&limit=` | bearer; `X-Total-Count`, `X-Page` and a `Link` to the next page |
| `POST /todos` | bearer; 422 with field errors on an empty title |
| `GET`, `PUT`, `DELETE /todos/{id}` | bearer; 404 when unknown |
| `POST /jobs`, `GET /jobs/{id}` | bearer; the state goes queued, running, done, one step per poll |
| `POST /upload` | `multipart/form-data`, echoes the parts |
| `POST /graphql` | bearer; `{"query","variables"}` with a `todos(done:)` field and `health` |
| `GET /reports/daily.csv` | `text/csv` |
| `GET /slow?ms=` | sleeps, capped at 10 s |
| `GET /status/{code}`, `GET /redirect` | any status; a 302 to `/status/200` |
| `GET /health` | version and uptime, no auth |

JSON bodies end in a newline and have `<`, `>` and `&` escaped. Request
bodies are read leniently: member names in any letter case, and whatever
follows the first JSON value is ignored.

## Tests

`src/tests.rs` covers the routes, runs every request of the embedded
project and its `features/` against a live instance, checks the project is
already in `apic fmt`'s form, and replays `testdata/extra.feature` through
`apic_bdd` in all four report formats against the expected reports in
`testdata/`.
