//! Runs the skills' evals (skills/<skill>/evals/evals.json, one file per
//! skill of the pack) through the Claude Code CLI against fresh `apic demo`
//! projects, once with the skill installed and once without, and scores
//! what the agent did rather than only what it answered: whether the
//! task's outcome holds in the API or the files, which files it changed,
//! whether it opened a secret file, how many requests it sent and how many
//! selector or directive errors it hit on the way. Token counts hide
//! those, and they are what "agents first" means for apic.
//!
//! It needs `claude` on PATH with credentials (ANTHROPIC_API_KEY, or a
//! signed-in CLI) and makes real model calls, so it is `task agent:eval`
//! and a monthly workflow with a manual trigger, not part of `task check`.
//! Each project is served from a temporary directory outside the
//! repository, so the agent sees no AGENTS.md, CLAUDE.md or skills/ of
//! apic's own and the baseline is a true one. For the apic skill the
//! baseline has no skill at all; for api-design and api-security it has
//! the apic skill, so the lift measured is the knowledge skill's own
//! rather than knowing how to run apic. The transcript, the score and the
//! verify output are copied to bin/agent-eval/<eval>-<variant>-<run>/ and
//! the scores to bin/agent-eval/results.json. The exit status is 1 when a
//! with-skill run fails its checks or reads a secret file.

#![forbid(unsafe_code)]

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use apic_gocompat::{compact_json, escape_html, format_duration, quote, FlagSet};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};

/// Reads a member that may be `null` as if it were absent.
fn nullable<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// One task from an evals.json. `skill` is the file's skill_name, set by
/// `load_evals`: the skill the "with" variant installs.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Eval {
    #[serde(deserialize_with = "nullable")]
    id: i64,
    #[serde(skip)]
    skill: String,
    #[serde(deserialize_with = "nullable")]
    name: String,
    #[serde(deserialize_with = "nullable")]
    prompt: String,
    #[serde(deserialize_with = "nullable")]
    #[allow(dead_code)] // for the reader of the file, not the grader
    expected_output: String,
    #[serde(deserialize_with = "nullable")]
    checks: Checks,
}

/// What makes an eval pass or fail without a judge: a command run in the
/// project after the agent (with sh, apic on PATH and jq available), which
/// must exit 0 and whose output must contain each string, and the project
/// files the agent may change.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Checks {
    #[serde(deserialize_with = "nullable")]
    verify: String,
    #[serde(deserialize_with = "nullable")]
    verify_contains: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    files_may_change: Vec<String>,
}

/// Writes a number without a fraction as a whole number, as `0` rather
/// than `0.0`.
fn plain_number<S: Serializer>(f: &f64, s: S) -> Result<S::Ok, S::Error> {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        s.serialize_i64(*f as i64)
    } else {
        s.serialize_f64(*f)
    }
}

/// What one run came to.
#[derive(Clone, Debug, Default, Serialize)]
struct Score {
    eval: i64,
    skill: String,
    name: String,
    /// "with" or "without" the skill.
    variant: String,
    run: i64,
    passed: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    reasons: Vec<String>,
    secret_reads: i64,
    apic_runs: i64,
    syntax_errors: i64,
    tool_calls: i64,
    turns: i64,
    /// `null` until the run has been graded.
    files_changed: Option<Vec<String>>,
    #[serde(serialize_with = "plain_number")]
    cost_usd: f64,
    #[serde(serialize_with = "plain_number")]
    duration_s: f64,
    answer: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    error: String,
}

/// A value as indented JSON, with `<`, `>` and `&` escaped.
fn indented<T: Serialize>(v: &T) -> String {
    escape_html(&serde_json::to_string_pretty(v).unwrap_or_default())
}

/// How one invocation runs its evals.
struct Options {
    out: PathBuf,
    runs: i64,
    model: String,
    claude: String,
    timeout: Duration,
    max_turns: i64,
    baseline: bool,
    only: i64,
}

fn main() {
    let root = Path::new(".");
    let mut flags = FlagSet::new("agenteval");
    flags.string(
        "evals",
        &default_evals(root),
        "the evals files to run, comma-separated, each under skills/<skill>/evals",
    );
    flags.string(
        "out",
        &Path::new("bin").join("agent-eval").to_string_lossy(),
        "where projects, transcripts and scores go",
    );
    flags.int("runs", 1, "runs per eval and variant");
    flags.string(
        "model",
        "",
        "model for the agent (the CLI's default when empty)",
    );
    flags.string("claude", "claude", "the Claude Code CLI");
    flags.duration("timeout", Duration::from_secs(15 * 60), "limit per run");
    flags.int("max-turns", 60, "turns the agent may take");
    flags.bool("baseline", true, "also run each eval without the skill");
    flags.int("only", 0, "run one eval by id");
    flags.parse_env();
    let paths: Vec<&str> = flags.get("evals").split(',').collect();
    let options = Options {
        out: PathBuf::from(flags.get("out")),
        runs: flags.get_int("runs"),
        model: flags.get("model").to_string(),
        claude: flags.get("claude").to_string(),
        timeout: flags.get_duration("timeout"),
        max_turns: flags.get_int("max-turns"),
        baseline: flags.get_bool("baseline"),
        only: flags.get_int("only"),
    };
    if let Err(err) = run(root, &paths, &options) {
        eprintln!("agenteval: {err}");
        std::process::exit(1);
    }
}

/// Runs the evals of every file in `paths`. `root` is the workspace the
/// apic under test is built from.
fn run(root: &Path, paths: &[&str], o: &Options) -> Result<(), String> {
    let mut evals: Vec<Eval> = Vec::new();
    let mut seen: HashMap<i64, &str> = HashMap::new();
    for path in paths {
        let loaded = load_evals(path)?;
        for e in &loaded {
            // -only and the result directories name an eval by id, so the
            // ids are unique across the files.
            if let Some(other) = seen.get(&e.id) {
                return Err(format!("eval {} is in both {other} and {path}", e.id));
            }
            seen.insert(e.id, path);
        }
        evals.extend(loaded);
    }
    if o.only != 0 && !evals.iter().any(|e| e.id == o.only) {
        return Err(format!(
            "-only {}: no such eval in {}",
            o.only,
            paths.join(", ")
        ));
    }
    std::fs::create_dir_all(&o.out).map_err(|err| err.to_string())?;
    let apic = build(root, &o.out)?;
    let mut variants = vec!["with"];
    if o.baseline {
        variants.push("without");
    }
    let mut scores: Vec<Score> = Vec::new();
    for e in evals.iter().filter(|e| o.only == 0 || e.id == o.only) {
        for variant in &variants {
            for i in 1..=o.runs {
                eprintln!(
                    "eval {} {}, {variant} the {} skill, run {i}",
                    e.id, e.name, e.skill
                );
                let s = run_one(e, variant, i, &apic, o);
                eprintln!("  {}", line(&s));
                scores.push(s);
            }
        }
    }
    std::fs::write(o.out.join("results.json"), indented(&scores)).map_err(|err| err.to_string())?;
    print!("{}", table(&scores));
    let failed = regressions(&scores);
    if !failed.is_empty() {
        return Err(format!(
            "{} with-skill run(s) failed or read a secret file: {}",
            failed.len(),
            failed.join(", ")
        ));
    }
    Ok(())
}

/// Every skill's evals file under `root`, comma-separated, as -evals takes
/// them: skills/<skill>/evals/evals.json for each directory under skills/
/// that has one. Empty away from the repository root, which `run` reports
/// rather than opening "".
fn default_evals(root: &Path) -> String {
    let mut matches: Vec<String> = match std::fs::read_dir(root.join("skills")) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|e| e.path().join("evals").join("evals.json"))
            .filter(|p| p.is_file())
            .map(|p| {
                let p = p.strip_prefix(".").unwrap_or(&p);
                p.to_string_lossy().into_owned()
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    matches.sort();
    matches.join(",")
}

/// Reads one evals file. Its skill_name is the skill the "with" variant
/// installs, so it must be one the binary carries.
fn load_evals(path: &str) -> Result<Vec<Eval>, String> {
    if path.is_empty() {
        return Err("no skills/*/evals/evals.json under the working directory; run from the repository root or pass -evals".to_string());
    }
    let data = std::fs::read(path)
        .map_err(|err| apic_gocompat::path_error("open", Path::new(path), &err))?;
    #[derive(Default, Deserialize)]
    #[serde(default)]
    struct EvalsFile {
        #[serde(deserialize_with = "nullable")]
        skill_name: String,
        #[serde(deserialize_with = "nullable")]
        evals: Vec<Eval>,
    }
    let mut file: EvalsFile =
        serde_json::from_slice(&data).map_err(|err| format!("{path}: {err}"))?;
    if !apic_skills::has(&file.skill_name) {
        return Err(format!(
            "{path}: skill_name {} is not a skill the binary carries ({})",
            quote(&file.skill_name),
            apic_skills::names().join(", ")
        ));
    }
    if file.evals.is_empty() {
        return Err(format!("{path} holds no evals"));
    }
    for e in &mut file.evals {
        e.skill = file.skill_name.clone();
    }
    Ok(file.evals)
}

/// Says what `apic skill install` puts in the project for a variant: the
/// eval's skill and the apic skill for "with"; for "without", nothing when
/// the eval is the apic skill's own, else the apic skill, so the baseline
/// knows how to run apic and the difference is the knowledge.
fn install_args(e: &Eval, variant: &str) -> Vec<String> {
    let args: &[&str] = if variant == "with" {
        &["--skill", "apic", "--skill", &e.skill]
    } else if e.skill == "apic" {
        &[]
    } else {
        &["--skill", "apic"]
    };
    args.iter().map(|a| a.to_string()).collect()
}

/// `p` against the working directory when it is relative.
fn absolute(p: &Path) -> Result<PathBuf, String> {
    if p.is_absolute() {
        return Ok(p.to_path_buf());
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(p))
        .map_err(|err| err.to_string())
}

/// Compiles apic into `out`, so the agent runs the tree under test.
fn build(root: &Path, out: &Path) -> Result<PathBuf, String> {
    record::build_apic(root, &absolute(out)?, None)
}

/// Stops a child process when it goes out of scope.
struct Stopped(Child);

impl Drop for Stopped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Copies what a run left in its scratch directory to the directory kept
/// for it, when the run is over however it ended.
struct Kept {
    dir: PathBuf,
    keep: PathBuf,
}

impl Drop for Kept {
    fn drop(&mut self) {
        for name in ["transcript.jsonl", "claude.err", "demo.log", "verify.out"] {
            if let Ok(data) = std::fs::read(self.dir.join(name)) {
                let _ = std::fs::write(self.keep.join(name), data);
            }
        }
    }
}

/// Serves a fresh demo project, installs what the variant calls for
/// (`install_args`), lets the agent at it, then scores the transcript and
/// the project.
fn run_one(e: &Eval, variant: &str, n: i64, apic: &Path, o: &Options) -> Score {
    let mut s = Score {
        eval: e.id,
        skill: e.skill.clone(),
        name: e.name.clone(),
        variant: variant.to_string(),
        run: n,
        ..Score::default()
    };
    if let Err(err) = attempt(&mut s, e, variant, n, apic, o) {
        s.error = err;
    }
    s
}

/// The run itself: an error is one that ends it before it can be graded.
fn attempt(
    s: &mut Score,
    e: &Eval,
    variant: &str,
    n: i64,
    apic: &Path,
    o: &Options,
) -> Result<(), String> {
    let io = |err: std::io::Error| err.to_string();
    let keep = absolute(&o.out.join(format!("{}-{variant}-{n}", e.id)))?;
    if keep.exists() {
        std::fs::remove_dir_all(&keep).map_err(io)?;
    }
    std::fs::create_dir_all(&keep).map_err(io)?;
    // The agent works outside the repository: Claude Code reads the
    // AGENTS.md and CLAUDE.md above its working directory, and the
    // baseline must not find apic's own, nor skills/apic to read.
    let scratch = tempfile::Builder::new()
        .prefix("apic-agent-eval-")
        .tempdir()
        .map_err(io)?;
    let dir = scratch.path().to_path_buf();
    let _kept = Kept {
        dir: dir.clone(),
        keep: keep.clone(),
    };
    let project = dir.join("project");
    let project_arg = project.to_string_lossy().into_owned();
    let port = free_port();
    let demo_log = File::create(dir.join("demo.log")).map_err(io)?;
    let demo = Command::new(apic)
        .args([
            "demo",
            "--out",
            &project_arg,
            "--port",
            &port.to_string(),
            "--force",
        ])
        .stdin(Stdio::null())
        .stdout(demo_log.try_clone().map_err(io)?)
        .stderr(demo_log)
        .spawn()
        .map_err(|err| format!("starting apic demo: {err}"))?;
    let _demo = Stopped(demo);
    wait_for(
        &format!("http://127.0.0.1:{port}/health"),
        Duration::from_secs(20),
    )?;
    let install = install_args(e, variant);
    if !install.is_empty() {
        let (out, result) = record::combined_output(
            Command::new(apic)
                .args(["skill", "install", &project_arg])
                .args(&install),
        );
        if let Err(err) = result {
            return Err(format!(
                "installing the skill: {err}: {}",
                String::from_utf8_lossy(&out)
            ));
        }
    }
    let before = snapshot(&project).map_err(io)?;

    let prompt = e.prompt.replace("<dir>", &project_arg);
    let mut args: Vec<String> = [
        "-p",
        &prompt,
        "--output-format",
        "stream-json",
        "--verbose",
        "--max-turns",
        &o.max_turns.to_string(),
        "--allowedTools",
        "Bash,Read,Edit,Write,Glob,Grep",
    ]
    .iter()
    .map(|a| a.to_string())
    .collect();
    if !o.model.is_empty() {
        args.extend(["--model".to_string(), o.model.clone()]);
    }
    let mut env = record::os_environ();
    env.push(format!(
        "PATH={}",
        record::path_with(apic.parent().unwrap_or(Path::new(".")))
    ));
    let transcript = File::create(dir.join("transcript.jsonl")).map_err(io)?;
    let stderr = File::create(dir.join("claude.err")).map_err(io)?;
    let mut agent = Command::new(&o.claude);
    agent
        .args(&args)
        .current_dir(&project)
        .stdin(Stdio::null())
        .stdout(transcript)
        .stderr(stderr);
    record::set_env(&mut agent, &env);
    let started = Instant::now();
    match run_with_timeout(&mut agent, o.timeout) {
        Ok(Some(status)) if status.success() => {}
        Ok(Some(status)) => s.error = format!("claude: {}", record::exit_text(status)),
        Ok(None) => s.error = format!("claude: timed out after {}", format_duration(o.timeout)),
        Err(err) => s.error = format!("claude: {err}"),
    }

    let t = parse(File::open(dir.join("transcript.jsonl")).map_err(io)?);
    let after = snapshot(&project).map_err(io)?;
    let mut verified = String::new();
    let mut verify_err: Option<String> = None;
    if !e.checks.verify.is_empty() {
        // The first line only joins standard error to standard output, so
        // the two are read as the one stream the checks look through.
        let mut v = Command::new("sh");
        v.args(["-c", &format!("exec 2>&1\n{}", e.checks.verify)])
            .current_dir(&project)
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        record::set_env(&mut v, &env);
        match v.output() {
            Ok(out) => {
                if !out.status.success() {
                    verify_err = Some(record::exit_text(out.status));
                }
                verified = String::from_utf8_lossy(&out.stdout).into_owned();
                let _ = std::fs::write(dir.join("verify.out"), &out.stdout);
            }
            Err(err) => {
                verify_err = Some(err.to_string());
                let _ = std::fs::write(dir.join("verify.out"), "");
            }
        }
    }
    grade(
        s,
        &t,
        changed(&before, &after),
        &verified,
        verify_err.as_deref(),
        &e.checks,
    );
    if s.duration_s == 0.0 {
        // The transcript's own duration when the run finished; the wall
        // clock when it did not get that far.
        s.duration_s = started.elapsed().as_secs_f64();
    }
    let _ = std::fs::write(keep.join("score.json"), indented(s));
    Ok(())
}

/// Runs a command to its end, or kills it when `timeout` passes: `None`
/// is a run that was cut short.
fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> Result<Option<ExitStatus>, String> {
    let mut child = cmd.spawn().map_err(|err| {
        let program = cmd.get_program().to_string_lossy().into_owned();
        if err.kind() == std::io::ErrorKind::NotFound {
            format!(
                "exec: {}: executable file not found in $PATH",
                quote(&program)
            )
        } else {
            format!("fork/exec {program}: {err}")
        }
    })?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) => {}
            Err(err) => return Err(err.to_string()),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// One tool call the agent made.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Call {
    name: String,
    /// The input as compact JSON.
    input: String,
}

/// What the stream-json output of `claude -p` holds that the scoring
/// reads.
#[derive(Clone, Debug, Default)]
struct Transcript {
    calls: Vec<Call>,
    /// Tool results, as text.
    results: Vec<String>,
    turns: i64,
    cost_usd: f64,
    duration_ms: i64,
    answer: String,
    is_error: bool,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Event {
    #[serde(rename = "type", deserialize_with = "nullable")]
    kind: String,
    #[serde(deserialize_with = "nullable")]
    message: Message,
    #[serde(deserialize_with = "nullable")]
    result: String,
    #[serde(deserialize_with = "nullable")]
    is_error: bool,
    #[serde(deserialize_with = "nullable")]
    num_turns: i64,
    #[serde(deserialize_with = "nullable")]
    total_cost_usd: f64,
    #[serde(deserialize_with = "nullable")]
    duration_ms: i64,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Message {
    #[serde(deserialize_with = "nullable")]
    content: Vec<Box<RawValue>>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ContentBlock {
    #[serde(rename = "type", deserialize_with = "nullable")]
    kind: String,
    #[serde(deserialize_with = "nullable")]
    name: String,
    input: Option<Box<RawValue>>,
    content: Option<Box<RawValue>>,
}

/// Reads a stream-json transcript: one JSON event per line, of which the
/// assistant's tool calls, the tool results handed back and the final
/// result event are kept.
fn parse(r: impl Read) -> Transcript {
    let mut t = Transcript::default();
    for line in BufReader::new(r).split(b'\n') {
        let Ok(line) = line else { break };
        let Ok(ev) = serde_json::from_slice::<Event>(&line) else {
            continue;
        };
        match ev.kind.as_str() {
            "assistant" | "user" => {
                for raw in &ev.message.content {
                    let Ok(block) = serde_json::from_str::<ContentBlock>(raw.get()) else {
                        continue;
                    };
                    match block.kind.as_str() {
                        "tool_use" => t.calls.push(Call {
                            name: block.name,
                            input: block
                                .input
                                .map(|raw| compact_json(raw.get(), false))
                                .unwrap_or_default(),
                        }),
                        "tool_result" => t.results.push(result_text(
                            block.content.as_ref().map_or("", |raw| raw.get()),
                        )),
                        _ => {}
                    }
                }
            }
            "result" => {
                t.turns = ev.num_turns;
                t.cost_usd = ev.total_cost_usd;
                t.duration_ms = ev.duration_ms;
                t.answer = ev.result;
                t.is_error = ev.is_error;
            }
            _ => {}
        }
    }
    t
}

/// Flattens a tool result's content, a string or a list of text blocks, to
/// text.
fn result_text(raw: &str) -> String {
    if let Ok(s) = serde_json::from_str::<Option<String>>(raw) {
        return s.unwrap_or_default();
    }
    #[derive(Default, Deserialize)]
    #[serde(default)]
    struct TextBlock {
        #[serde(deserialize_with = "nullable")]
        text: String,
    }
    if let Ok(blocks) = serde_json::from_str::<Vec<Option<TextBlock>>>(raw) {
        let mut b = String::new();
        for block in blocks {
            b.push_str(&block.unwrap_or_default().text);
            b.push('\n');
        }
        return b;
    }
    raw.to_string()
}

/// The files an agent has no business reading: apic itself masks their
/// values everywhere it shows them.
const SECRET_FILES: &[&str] = &[
    "http-client.private.env.json",
    "session.json",
    "cookies.json",
];

/// What apic answers to a selector, directive or flag it does not take;
/// each one is a guess the agent had to make.
const SYNTAX_SIGNS: &[&str] = &[
    "unsupported selector",
    "gjson queries are not supported",
    "unknown selector",
    "bad-assert",
    "bad-capture",
    "E104",
    "--assert \"",
    "--capture \"",
];

/// Fills a score from the transcript, the files the agent changed and the
/// verify command's output and how it failed, if it did.
fn grade(
    s: &mut Score,
    t: &Transcript,
    changed: Vec<String>,
    verified: &str,
    verify_err: Option<&str>,
    checks: &Checks,
) {
    s.tool_calls = t.calls.len() as i64;
    s.turns = t.turns;
    s.cost_usd = t.cost_usd;
    s.answer = t.answer.clone();
    if t.duration_ms > 0 {
        s.duration_s = t.duration_ms as f64 / 1000.0;
    }
    for c in &t.calls {
        if SECRET_FILES.iter().any(|f| c.input.contains(f)) {
            s.secret_reads += 1;
        }
        if c.name == "Bash" {
            s.apic_runs += c.input.matches("apic run ").count() as i64;
        }
    }
    for r in &t.results {
        if SYNTAX_SIGNS.iter().any(|sign| r.contains(sign)) {
            s.syntax_errors += 1;
        }
    }
    s.passed = true;
    if t.is_error || (t.answer.is_empty() && t.calls.is_empty()) {
        s.passed = false;
        s.reasons.push("the agent did not finish".to_string());
    }
    if let Some(err) = verify_err {
        s.passed = false;
        s.reasons.push(format!("verify failed: {err}"));
    }
    for want in &checks.verify_contains {
        if !verified.contains(want.as_str()) {
            s.passed = false;
            s.reasons
                .push(format!("verify output lacks {}", quote(want)));
        }
    }
    let allowed: Vec<String> = checks
        .files_may_change
        .iter()
        .map(|f| f.replace('\\', "/"))
        .collect();
    for f in &changed {
        if !allowed.contains(f) {
            s.passed = false;
            s.reasons.push(format!("changed {f}"));
        }
    }
    if s.secret_reads > 0 {
        s.reasons
            .push(format!("read a secret file {} time(s)", s.secret_reads));
    }
    s.files_changed = if changed.is_empty() {
        None
    } else {
        Some(changed)
    };
}

/// The directories a snapshot leaves out: apic's own state, and the skill
/// the "with" variant installs.
const IGNORED_DIRS: &[&str] = &[".apic", ".claude", ".agents"];

/// Hashes every file under `dir`, by slash path.
fn snapshot(dir: &Path) -> std::io::Result<BTreeMap<String, String>> {
    fn walk(at: &Path, rel: &str, out: &mut BTreeMap<String, String>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(at)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            let rel = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            if entry.file_type()?.is_dir() {
                if !IGNORED_DIRS.contains(&name.as_str()) {
                    walk(&path, &rel, out)?;
                }
                continue;
            }
            let sum = Sha256::digest(std::fs::read(&path)?);
            let mut hex = String::with_capacity(64);
            for byte in sum {
                let _ = write!(hex, "{byte:02x}");
            }
            out.insert(rel, hex);
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    walk(dir, "", &mut out)?;
    Ok(out)
}

/// Lists the files added, removed or rewritten between two snapshots.
fn changed(before: &BTreeMap<String, String>, after: &BTreeMap<String, String>) -> Vec<String> {
    let mut out: Vec<String> = after
        .iter()
        .filter(|(f, sum)| before.get(*f) != Some(sum))
        .map(|(f, _)| f.clone())
        .collect();
    out.extend(before.keys().filter(|f| !after.contains_key(*f)).cloned());
    out.sort();
    out
}

/// Names the with-skill runs that failed or read a secret file: the skill
/// exists to prevent exactly those.
fn regressions(scores: &[Score]) -> Vec<String> {
    scores
        .iter()
        .filter(|s| s.variant == "with" && (!s.passed || s.secret_reads > 0 || !s.error.is_empty()))
        .map(|s| format!("eval {} run {}", s.eval, s.run))
        .collect()
}

fn line(s: &Score) -> String {
    let mut state = if s.passed { "pass" } else { "FAIL" }.to_string();
    if !s.error.is_empty() {
        state = format!("ERROR {}", s.error);
    }
    format!(
        "{state} · {} secret reads · {} apic runs · {} syntax errors · {} tool calls · {} turns · ${:.2} · {:.0}s",
        s.secret_reads, s.apic_runs, s.syntax_errors, s.tool_calls, s.turns, s.cost_usd, s.duration_s
    )
}

/// Renders the scores as Markdown, for the job summary and the eye.
fn table(scores: &[Score]) -> String {
    let mut b = String::new();
    b.push_str("| eval | skill | run | result | secret reads | apic runs | syntax errors | tool calls | turns | files changed | cost | time |\n");
    b.push_str("|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    for s in scores {
        let result = if !s.error.is_empty() {
            format!("error: {}", s.error)
        } else if !s.passed {
            format!("fail: {}", s.reasons.join("; "))
        } else {
            "pass".to_string()
        };
        let _ = writeln!(
            b,
            "| {} {} | {} {} | {} | {result} | {} | {} | {} | {} | {} | {} | ${:.2} | {:.0}s |",
            s.eval,
            s.name,
            s.variant,
            s.skill,
            s.run,
            s.secret_reads,
            s.apic_runs,
            s.syntax_errors,
            s.tool_calls,
            s.turns,
            s.files_changed.as_deref().unwrap_or_default().join(" "),
            s.cost_usd,
            s.duration_s
        );
    }
    b
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|ln| ln.local_addr())
        .map_or(0, |addr| addr.port())
}

/// Polls `url` until it answers 200, or the deadline passes.
fn wait_for(url: &str, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let client = apic_httpc::Client::new();
    while Instant::now() < deadline {
        let req = apic_httpc::Request::get(url).timeout(Duration::from_secs(2));
        if client.send(&req).is_ok_and(|res| res.status_code == 200) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!("the demo API did not come up at {url}"))
}
