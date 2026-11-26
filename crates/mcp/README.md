# apic-mcp

Exposes a project's requests to AI agents over the Model Context Protocol
(`apic_mcp`): the server behind `apic mcp`. The protocol is implemented
here, with no SDK: JSON-RPC 2.0, one message per line on stdio, and the
streamable HTTP transport on `apic_httpd`.

```rust
let cfg = apic_mcp::Config { dir: ".".into(), env: "dev".into(), version: "1.2.3".into() };
apic_mcp::serve(&cancel, cfg.clone())?;                                  // stdio
apic_mcp::serve_http(&cancel, cfg, "127.0.0.1:8765", "", Some(&|addr| eprintln!("{addr}")))?;
```

## API

| Item | Signature / notes |
| --- | --- |
| `Config { dir, env, version }` | The project root, the default environment, the version reported as `serverInfo.version` and sent as the User-Agent |
| `new(Config)` | `-> Result<Server, apic_runner::Error>`: loads the project once to list its `.http` files as resources (`Server` is `Clone`) |
| `Server::run(&CancelToken, impl BufRead, impl Write + Send + 'static)` | `-> Result<(), String>`: serves one client over any reader and writer. `Ok` when the input ends; `Err` with the reason when a line is not a message (bad JSON, no `"jsonrpc":"2.0"`, an id that is neither number nor string, a batch on 2025-06-18 or later) |
| `serve(&CancelToken, Config)` | `-> Result<(), Error>`: `new` then `run` over stdin and stdout |
| `check_bind(addr, token)` | `-> Result<(), String>`: refuses an address outside the loopback interface unless a token guards the server |
| `handler(Config, token)` | `-> Result<apic_httpd::Handler, Error>`: the streamable HTTP transport, behind a bearer token when one is given |
| `serve_http(&CancelToken, Config, addr, token, ready)` | `-> Result<(), Error>`: `check_bind`, bind, call `ready` with the bound address, serve until cancelled |

## What the server answers

- `initialize`: capabilities `logging`, `resources.listChanged`,
  `tools.listChanged`; the instructions; the client's protocol version
  when it is one of 2025-11-25, 2025-06-18, 2025-03-26 or 2024-11-05, else
  2025-11-25. Before it only `ping` is answered; a second one is an error.
- `ping`, `logging/setLevel`: `{}`.
- `tools/list`: nine tools in name order, each with `description`,
  `inputSchema` and `name` (`testdata/tools_list.json` is the exact text).
- `tools/call`: arguments are checked against the tool's schema first
  (`validating "arguments": validating root: required: missing
  properties: ["name"]` as an `isError` result). A result is
  `{"content":[{"type":"text","text":<json>}],"structuredContent":<json>}`;
  a failed call is `isError` with the text `<message>\n(<code> <title>:
  <hint>)` and `structuredContent` `{"error": <apic_runner::ErrorInfo>}`.
  An unknown tool is JSON-RPC error -32602.
- `resources/list`: the project's `.http` files as `file://` URIs, as
  they were when the server started. `resources/read` serves a listed
  file only, re-checked against the project on disk; anything else is
  -32602 `Resource not found` with `data.uri`.
- `resources/templates/list`, `prompts/list`: empty lists. List results
  carry `"ttlMs":0,"cacheScope":"public"`; a cursor is -32602.
- Anything else: -32601 `method not found: "<method>"`. Missing `params`
  is -32600; a lifecycle error has code 0.
- Calls run on threads of their own; `notifications/cancelled` cancels
  the call it names, which then answers with the cancelled error.

The tools: `list_requests`, `describe_request`, `run_request`, `run_file`,
`list_environments`, `clear_session`, `run_features`, `validate_project`,
`curl_request`. Each call loads the project afresh, so edits are seen
without a restart.

## Streamable HTTP

POST takes `Content-Type: application/json` and an `Accept` with both
`application/json` and `text/event-stream`. `initialize` opens a session,
named in the response's `Mcp-Session-Id`; later requests send that header
(404 `session not found` otherwise). Calls are answered as an event
stream, one `event: message` per response; notifications and responses
with 202. GET opens the session's stream for server messages (one at a
time: a second is 409) and DELETE ends the session. A batch is accepted
when `Mcp-Protocol-Version` is absent or before 2025-06-18. A request
from this machine whose `Host` is not a loopback name is 403, and one for
protocol 2026-07-28, which has no sessions, is refused with -32022.

## Tests

`src/tests.rs` drives every tool through a session against a local API,
the stdio loop over in-memory pipes, and the HTTP transport through a
test server; `testdata/` holds the expected `initialize` and `tools/list`
responses.
