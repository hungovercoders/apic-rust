//! The tools: what each one takes, the checks on what it is given, and
//! what it does with the project.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};

use apic_assert::Resolved as Schema;
use apic_gocompat::{escape_html, escape_line_separators, json_string, quote};
use apic_httpc::CancelToken;
use apic_httpfile::{Diagnostic, Request};
use apic_project::Project;
use apic_runner::{info, usage, Code, Error, Options as RunnerOptions, Runner, UsageError};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::Value;

use crate::protocol::{lock, RpcError, CODE_INVALID_PARAMS};
use crate::Inner;

struct Tool {
    name: &'static str,
    description: &'static str,
    /// The JSON Schema of the arguments, as `tools/list` gives it.
    input_schema: &'static str,
}

const ENV_DEFAULT: &str =
    r#""env":{"type":"string","description":"environment name; defaults to the server's --env"}"#;
const VARS_OVERRIDE: &str = r#""vars":{"type":"object","description":"variable overrides, highest precedence","additionalProperties":{"type":"string"}}"#;
const VARS_PLAIN: &str = r#""vars":{"type":"object","additionalProperties":{"type":"string"}}"#;

/// The tools in name order, which is the order they are listed in.
static TOOLS: LazyLock<Vec<(Tool, Vec<&'static str>, Schema)>> = LazyLock::new(|| {
    let object = |properties: &str, required: &str| {
        let mut schema = String::from(r#"{"type":"object""#);
        if !properties.is_empty() {
            schema.push_str(&format!(r#","properties":{{{properties}}}"#));
        }
        if !required.is_empty() {
            schema.push_str(&format!(r#","required":["{required}"]"#));
        }
        schema.push_str(r#","additionalProperties":false}"#);
        schema
    };
    let tools: Vec<(&'static str, &'static str, String)> = vec![
        (
            "clear_session",
            "Forget captured values for an environment (or all of them).",
            object(
                r#""env":{"type":"string"},"all":{"type":"boolean","description":"clear every environment"}"#,
                "",
            ),
        ),
        (
            "curl_request",
            "The curl command equivalent to a request (or with lang, HTTPie, PowerShell, Python requests, JavaScript fetch or Go), with its variables resolved. Credentials are shell placeholders and header, body and query values are masked unless raw is set, so the command can go into a log or a ticket as it is.",
            object(
                &format!(
                    r#""name":{{"type":"string","description":"request id from list_requests"}},{ENV_DEFAULT},{VARS_OVERRIDE},"raw":{{"type":"boolean","description":"include the live credentials and values, so the command runs as printed; by default they are placeholders and masks"}},"lang":{{"type":"string","description":"curl (default), httpie, powershell, python, js or go"}}"#
                ),
                "name",
            ),
        ),
        (
            "describe_request",
            "Show a request's variables and where each comes from, its captures and assertions, and whether it is ready to run.",
            object(
                &format!(
                    r#""name":{{"type":"string","description":"request id from list_requests, e.g. get-user or users.http#2"}},{ENV_DEFAULT}"#
                ),
                "name",
            ),
        ),
        (
            "list_environments",
            "List the environments in http-client.env.json and the variables in effect (secrets masked).",
            object(r#""env":{"type":"string"}"#, ""),
        ),
        (
            "list_requests",
            "List every request in the project with its id, method, URL template, file and description.",
            object("", ""),
        ),
        (
            "run_features",
            "Run Gherkin .feature files (default: features/ under the project) against the project's requests and return a pass/fail summary with the failing steps.",
            object(
                &format!(
                    r#""paths":{{"type":["null","array"],"items":{{"type":"string"}},"description":"feature files or directories relative to the project root; default features/"}},"tags":{{"type":"string","description":"tag expression such as @smoke && ~@slow"}},"env":{{"type":"string"}},{VARS_PLAIN},"use_session":{{"type":"boolean","description":"share .apic/session.json with run_request and later calls (captures flow both ways); by default every scenario runs in an isolated in-memory session"}}"#
                ),
                "",
            ),
        ),
        (
            "run_file",
            "Run every request in a .http file in order as a flow. Stops at the first failure unless keep_going is set.",
            object(
                &format!(
                    r#""file":{{"type":"string","description":"path of the .http file relative to the project root"}},"env":{{"type":"string"}},{VARS_PLAIN},"keep_going":{{"type":"boolean","description":"continue after a failed request"}}"#
                ),
                "file",
            ),
        ),
        (
            "run_request",
            "Send one request and return status, headers, body, captures and assertion results. Captured values persist for later calls.",
            object(
                &format!(
                    r#""name":{{"type":"string","description":"request id from list_requests"}},{ENV_DEFAULT},{VARS_OVERRIDE}"#
                ),
                "name",
            ),
        ),
        (
            "validate_project",
            "Parse every .http file and report problems (bad selectors, unknown directives, duplicate names, missing body files, # @ref cycles) with file, line, column and a code, without sending any request. The same shape as `apic validate --json`.",
            object("", ""),
        ),
    ];
    tools
        .into_iter()
        .map(|(name, description, schema)| {
            let value: Value = serde_json::from_str(&schema).expect("a valid schema");
            let resolved = Schema::new(&value).expect("a schema that resolves");
            let input_schema: &'static str = Box::leak(schema.into_boxed_str());
            // The properties in the order the schema declares them.
            let mut properties: Vec<(usize, &'static str)> = PROPERTY_NAMES
                .iter()
                .filter_map(|p| Some((input_schema.find(&format!("\"{p}\":{{"))?, *p)))
                .collect();
            properties.sort_unstable();
            (
                Tool {
                    name,
                    description,
                    input_schema,
                },
                properties.into_iter().map(|(_, p)| p).collect(),
                resolved,
            )
        })
        .collect()
});

/// Every property a tool takes.
const PROPERTY_NAMES: [&str; 11] = [
    "name",
    "env",
    "vars",
    "raw",
    "lang",
    "file",
    "keep_going",
    "all",
    "paths",
    "tags",
    "use_session",
];

/// Every tool as `tools/list` describes it.
pub(crate) fn tools_list() -> Vec<String> {
    TOOLS
        .iter()
        .map(|(tool, _, _)| {
            escape_html(&format!(
                "{{\"description\":{},\"inputSchema\":{},\"name\":\"{}\"}}",
                json_string(tool.description, false),
                tool.input_schema,
                tool.name
            ))
        })
        .collect()
}

/// JSON text as every result is written: `<`, `>` and `&` escaped.
fn marshal<T: Serialize>(v: &T) -> String {
    let text = serde_json::to_string(v).unwrap_or_else(|_| "null".to_string());
    escape_line_separators(&escape_html(&text))
}

fn text_content(text: &str) -> String {
    format!("{{\"type\":\"text\",\"text\":{}}}", json_string(text, true))
}

/// A result that carries its JSON both as text and as an object.
fn structured(json: &str) -> String {
    format!(
        "{{\"content\":[{}],\"structuredContent\":{json}}}",
        text_content(json)
    )
}

/// Reports a failed call with the error's catalogue code, as the CLI's
/// --json does: the text for a model to read, the object to branch on.
fn tool_error(err: &Error) -> String {
    let info = info(err);
    let text = format!("{err}\n({} {}: {})", info.code, info.title, info.hint);
    format!(
        "{{\"content\":[{}],\"structuredContent\":{{\"error\":{}}},\"isError\":true}}",
        text_content(&text),
        marshal(&info)
    )
}

/// What a tool hands back: a result, or the error to report as one.
type ToolResult = Result<String, Error>;

fn default_args() -> serde_json::Map<String, Value> {
    serde_json::Map::new()
}

/// Runs a tool. Arguments that do not fit the tool's schema, like a
/// failure of the tool itself, come back as a result marked `isError`;
/// only an unknown tool is a protocol error.
pub(crate) fn call_tool(
    inner: &Arc<Inner>,
    name: &str,
    arguments: Option<&RawValue>,
    cancel: &CancelToken,
) -> Result<String, RpcError> {
    let Some((_, properties, schema)) = TOOLS.iter().find(|(tool, _, _)| tool.name == name) else {
        return Err(RpcError::new(
            CODE_INVALID_PARAMS,
            format!("unknown tool {}", quote(name)),
        ));
    };
    let invalid = |why: String| {
        Ok(format!(
            "{{\"content\":[{}],\"isError\":true}}",
            text_content(&format!("validating \"arguments\": {why}"))
        ))
    };
    let args = match arguments.map(|raw| (raw, serde_json::from_str::<Value>(raw.get()))) {
        None | Some((_, Ok(Value::Null))) => default_args(),
        Some((_, Ok(Value::Object(obj)))) => obj,
        Some((raw, _)) => {
            return invalid(format!(
                "unmarshaling arguments: json: cannot unmarshal {} into Go value of type map[string]interface {{}}",
                quote(raw.get())
            ));
        }
    };
    // A property that does not fit is reported in the order the tool
    // declares its properties, so each is tried by itself first.
    for property in properties {
        let Some(value) = args.get(*property) else {
            continue;
        };
        let alone = Value::Object(serde_json::Map::from_iter([(
            (*property).to_string(),
            value.clone(),
        )]));
        if let Err(why) = schema.validate(&alone) {
            if why.starts_with(&format!(
                "validating root: validating /properties/{property}:"
            )) {
                return invalid(why);
            }
        }
    }
    let args = Value::Object(args);
    if let Err(why) = schema.validate(&args) {
        return invalid(why);
    }
    let service = Service { inner };
    let result = match name {
        "list_requests" => service.list_requests(),
        "describe_request" => service.describe_request(input(&args)),
        "run_request" => service.run_request(input(&args), cancel),
        "run_file" => service.run_file(input(&args), cancel),
        "list_environments" => service.list_environments(input(&args)),
        "clear_session" => service.clear_session(input(&args)),
        "run_features" => service.run_features(input(&args), cancel),
        "validate_project" => service.validate_project(),
        _ => service.curl_request(input(&args)),
    };
    Ok(result.unwrap_or_else(|err| tool_error(&err)))
}

/// Reads arguments the schema has already checked.
fn input<T: for<'de> Deserialize<'de> + Default>(args: &Value) -> T {
    serde_json::from_value(args.clone()).unwrap_or_default()
}

#[derive(Serialize)]
struct RequestSummary {
    id: String,
    method: String,
    url: String,
    file: String,
    line: usize,
    #[serde(skip_serializing_if = "String::is_empty")]
    description: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    captures: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    asserts: Vec<String>,
    /// `# @ref` and `# @forceRef` targets.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    refs: Vec<String>,
    /// `# @disabled`: run_file skips it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    disabled: bool,
}

fn summarize(r: &Request) -> RequestSummary {
    RequestSummary {
        id: r.id(),
        method: r.method.clone(),
        url: r.url.clone(),
        file: r.file.path.clone(),
        line: r.line,
        description: r.description.clone(),
        captures: r.captures.iter().map(|c| c.name.clone()).collect(),
        asserts: r.asserts.iter().map(|a| a.expr.clone()).collect(),
        refs: r.refs().into_iter().map(|r| r.id).collect(),
        disabled: r.disabled(),
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CurlInput {
    name: String,
    env: String,
    vars: BTreeMap<String, String>,
    raw: bool,
    lang: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DescribeInput {
    name: String,
    env: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RunInput {
    name: String,
    env: String,
    vars: BTreeMap<String, String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RunFileInput {
    file: String,
    env: String,
    vars: BTreeMap<String, String>,
    keep_going: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct EnvInput {
    env: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ClearInput {
    env: String,
    all: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FeaturesInput {
    paths: Option<Vec<String>>,
    tags: String,
    env: String,
    vars: BTreeMap<String, String>,
    use_session: bool,
}

struct Service<'a> {
    inner: &'a Arc<Inner>,
}

/// The one request a target names.
fn single(r: &Runner, target: &str) -> Result<Arc<Request>, Error> {
    let reqs = r.project.resolve(target).map_err(|err| {
        let err = Error::Project(err);
        Error::from(UsageError::wrapping(
            Code::UnknownRequest,
            err.to_string(),
            &err,
        ))
    })?;
    if reqs.len() != 1 {
        return Err(usage(
            Code::Ambiguous,
            format!(
                "{} names {} requests; use run_file for a whole file or {target}#<name> for one",
                quote(target),
                reqs.len()
            ),
        )
        .into());
    }
    Ok(reqs[0].clone())
}

impl Service<'_> {
    fn load(&self) -> Result<Arc<Project>, Error> {
        apic_project::load(&self.inner.root).map_err(Error::Project)
    }

    fn new_runner(
        &self,
        env: &str,
        vars: BTreeMap<String, String>,
        keep_going: bool,
    ) -> Result<Arc<Runner>, Error> {
        let p = self.load()?;
        let env = if env.is_empty() {
            self.inner.cfg.env.as_str()
        } else {
            env
        };
        apic_runner::set_version(&self.inner.cfg.version);
        let r = Runner::new(
            p,
            RunnerOptions {
                env: env.to_string(),
                vars,
                keep_going,
                ..RunnerOptions::default()
            },
        )?;
        Ok(Arc::new(r))
    }

    fn list_requests(&self) -> ToolResult {
        #[derive(Serialize)]
        struct Out {
            root: String,
            requests: Vec<RequestSummary>,
        }
        let p = self.load()?;
        Ok(structured(&marshal(&Out {
            root: p.root.clone(),
            requests: p.requests().iter().map(|r| summarize(r)).collect(),
        })))
    }

    fn validate_project(&self) -> ToolResult {
        #[derive(Serialize)]
        struct Out {
            ok: bool,
            files: usize,
            requests: usize,
            diagnostics: Vec<Diagnostic>,
        }
        let p = self.load()?;
        let diagnostics = p.validate();
        Ok(structured(&marshal(&Out {
            ok: !diagnostics.iter().any(|d| d.severity == "error"),
            files: p.files.len(),
            requests: p.requests().len(),
            diagnostics,
        })))
    }

    fn curl_request(&self, input: CurlInput) -> ToolResult {
        #[derive(Serialize)]
        struct Out {
            id: String,
            lang: String,
            command: String,
        }
        let r = self.new_runner(&input.env, input.vars, false)?;
        let req = single(&r, &input.name)?;
        let res = r.resolve(&req)?;
        let d = r.describe(&req);
        if !d.ready {
            let missing: Vec<String> = d
                .variables
                .iter()
                .filter(|v| v.missing)
                .map(|v| v.name.clone())
                .collect();
            return Err(r.missing_error(&req, &missing));
        }
        let lang = if input.lang.is_empty() {
            "curl".to_string()
        } else {
            input.lang
        };
        let command = apic_snippet::render(&lang, &res, !input.raw)
            .map_err(|err| Error::Other(err.to_string()))?;
        Ok(structured(&marshal(&Out {
            id: req.id(),
            lang,
            command,
        })))
    }

    fn describe_request(&self, input: DescribeInput) -> ToolResult {
        let r = self.new_runner(&input.env, BTreeMap::new(), false)?;
        let req = single(&r, &input.name)?;
        let mut d = r.describe(&req);
        for v in &mut d.variables {
            if v.secret {
                v.value = "***".to_string();
            }
        }
        Ok(structured(&marshal(&d)))
    }

    fn run_request(&self, input: RunInput, cancel: &CancelToken) -> ToolResult {
        let r = self.new_runner(&input.env, input.vars, false)?;
        let req = single(&r, &input.name)?;
        match r.run(cancel, &req) {
            (_, Some(err)) => Err(err),
            (Some(res), None) => Ok(structured(&res.to_json())),
            (None, None) => Ok(structured("null")),
        }
    }

    fn run_file(&self, input: RunFileInput, cancel: &CancelToken) -> ToolResult {
        let r = self.new_runner(&input.env, input.vars, input.keep_going)?;
        if !input.file.contains(".http") && !input.file.contains(".rest") {
            return Err(usage(
                Code::Flag,
                format!(
                    "{} is not a .http file; use run_request for a single request",
                    quote(&input.file)
                ),
            )
            .into());
        }
        let reqs = r.project.resolve(&input.file).map_err(|err| {
            let err = Error::Project(err);
            Error::from(UsageError::wrapping(
                Code::UnknownRequest,
                err.to_string(),
                &err,
            ))
        })?;
        let (results, run_err) = r.run_all(cancel, &reqs);
        let ok = run_err.is_none() && results.iter().all(|res| res.ok);
        let mut out = format!("{{\"ok\":{ok}");
        if let Some(err) = &run_err {
            let text = err.to_string();
            if !text.is_empty() {
                out.push_str(&format!(",\"error\":{}", json_string(&text, true)));
            }
        }
        let results: Vec<String> = results.iter().map(apic_runner::Result::to_json).collect();
        out.push_str(&format!(",\"results\":[{}]}}", results.join(",")));
        Ok(structured(&out))
    }

    fn list_environments(&self, input: EnvInput) -> ToolResult {
        #[derive(Serialize)]
        struct Out {
            environments: Vec<String>,
            #[serde(skip_serializing_if = "String::is_empty")]
            current: String,
            files: Vec<String>,
            variables: Vec<apic_runner::VarInfo>,
        }
        let r = self.new_runner(&input.env, BTreeMap::new(), false)?;
        let mut variables = r.env_vars();
        for v in &mut variables {
            if v.secret {
                v.value = "***".to_string();
            }
        }
        Ok(structured(&marshal(&Out {
            environments: r.envs.names(),
            current: r.opts.env.clone(),
            files: r.envs.found.clone(),
            variables,
        })))
    }

    fn clear_session(&self, input: ClearInput) -> ToolResult {
        let r = self.new_runner(&input.env, BTreeMap::new(), false)?;
        let target = if input.all {
            "*".to_string()
        } else {
            r.opts.env.clone()
        };
        if let Some(session) = &r.session {
            let mut store = lock(session);
            store.clear(&target);
            store.save().map_err(|err| Error::Other(err.to_string()))?;
        }
        let jar = apic_session::open_jar(std::path::Path::new(&r.project.root))
            .map_err(|err| Error::Other(err.to_string()))?;
        jar.clear(&target);
        jar.save().map_err(|err| Error::Other(err.to_string()))?;
        Ok(structured(&format!(
            "{{\"cleared\":{}}}",
            json_string(&target, true)
        )))
    }

    fn run_features(&self, input: FeaturesInput, cancel: &CancelToken) -> ToolResult {
        #[derive(Serialize)]
        struct Out {
            #[serde(flatten)]
            summary: Option<apic_bdd::Summary>,
            exit_code: i32,
            #[serde(skip_serializing_if = "String::is_empty")]
            error: String,
        }
        let p = self.load()?;
        let env = [
            input.env.as_str(),
            self.inner.cfg.env.as_str(),
            p.config.env.as_str(),
        ]
        .into_iter()
        .find(|e| !e.is_empty())
        .unwrap_or("")
        .to_string();
        apic_runner::set_version(&self.inner.cfg.version);
        let opts = apic_bdd::Options {
            paths: input.paths.unwrap_or_default(),
            tags: input.tags,
            ..apic_bdd::Options::new(apic_bdd::Config {
                env,
                vars: input.vars,
                use_session: input.use_session,
                ..apic_bdd::Config::new(p)
            })
        };
        let (summary, _, exit_code, err) = apic_bdd::run_summary(cancel, &opts);
        let mut out = Out {
            summary,
            exit_code,
            error: String::new(),
        };
        if let Some(err) = err {
            let Some(summary) = &mut out.summary else {
                return Err(err);
            };
            summary.ok = false;
            out.error = err.to_string();
        }
        Ok(structured(&marshal(&out)))
    }
}
