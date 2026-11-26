//! apic mock: serve an OpenAPI 3 document as a mock API.

use apic_openapi::Route;
use apic_runner::Code;
use serde::Serialize;

use crate::io::wln;
use crate::root::{fail, App, CliResult, Invocation};
use crate::serve::{listen_local, serve_local};
use crate::shared::{plural, read_file};
use crate::spec::{ArgRule, CommandSpec, FlagSpec};

pub(crate) fn spec() -> CommandSpec {
    CommandSpec::new(
        "mock <openapi.yaml|openapi.json>",
        "Serve an OpenAPI 3 document as a mock API, so requests can run before the backend exists",
    )
    .long(
        "mock reads an OpenAPI 3 document and serves every operation in it with the
response the document declares: the first 2xx (else default, else the first),
a body from its example or built from its schema, and its headers. It keeps
no state and validates nothing: it is the document read back, so the
requests apic import wrote from the same document run green against it
before the API exists.

A Prefer header line on a request picks another documented response:
Prefer: code=404 serves that response (its class, or default, when the exact
code is not declared), and Prefer: example=name one of a media type's named
examples. A choice the document does not offer is a 406 with the reason.

The paths are served as written and, when the first server URL has a path
such as /v1, under that prefix too. The server listens on localhost only.",
    )
    .example(
        "  apic mock openapi.yaml                       # terminal 1
  apic import openapi.yaml -o api              # terminal 2: the requests it describes
  apic run list-pets -C api --var baseUrl=http://localhost:8090",
    )
    .args(ArgRule::Exact(1))
    .flag(FlagSpec::int("port", 8090, "port to serve the mock API on"))
    .runs(run)
}

fn run(a: &mut App, inv: &Invocation) -> CliResult {
    let spec = &inv.args[0];
    let port = inv.flags.int("port");
    let data = read_file(spec).map_err(|e| fail(Code::File, e))?;
    let m = apic_openapi::Mock::new(&data).map_err(|e| fail(Code::Mock, format!("{spec}: {e}")))?;
    let listener = listen_local(port)?;
    let url = format!("http://localhost:{port}");
    if a.g.json {
        #[derive(Serialize)]
        struct Out<'a> {
            spec: &'a str,
            url: &'a str,
            #[serde(serialize_with = "crate::shared::null_when_empty")]
            routes: &'a [Route],
            #[serde(skip_serializing_if = "<[String]>::is_empty")]
            skipped: &'a [String],
            listening: bool,
        }
        a.write_json(&Out {
            spec,
            url: &url,
            routes: &m.routes,
            skipped: &m.skipped,
            listening: true,
        })?;
    } else {
        wln!(
            a.stdout,
            "mock api listening on {url} · {} from {spec}",
            plural(m.routes.len(), "route")
        );
        // The routes in columns, two spaces apart.
        let method_width = m
            .routes
            .iter()
            .map(|r| r.method.chars().count())
            .max()
            .unwrap_or(0);
        let path_width = m
            .routes
            .iter()
            .map(|r| r.path.chars().count())
            .max()
            .unwrap_or(0);
        for r in &m.routes {
            let method_pad = " ".repeat(method_width - r.method.chars().count() + 2);
            let path_pad = " ".repeat(path_width - r.path.chars().count() + 2);
            wln!(
                a.stdout,
                "  {}{method_pad}{}{path_pad}{}",
                r.method,
                r.path,
                r.status
            );
        }
        for s in &m.skipped {
            wln!(a.stdout, "skipped {s}");
        }
        wln!(a.stdout, "try: apic run <request> --var baseUrl={url}");
    }
    serve_local(&inv.cancel, listener, m.into_handler());
    Ok(())
}
