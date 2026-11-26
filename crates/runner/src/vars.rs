//! Where a placeholder's value comes from: the precedence layers of plain
//! variables, the built-ins and the response references.

use std::sync::{Arc, LazyLock};

use apic_gocompat::{format_time, quote, Time, RFC1123, RFC3339};
use apic_httpfile::Request;
use apic_template::RenderError;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::errors::Error;
use crate::localtime::LocalZone;
use crate::Runner;

/// Describes where a variable's value came from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VarInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub value: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub secret: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub missing: bool,
    /// The request that would provide it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub captured_by: String,
    /// `captured_by` is a `# @ref` of the request, so it runs first.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ref_runs: bool,
}

/// What resolving one `{{expr}}` gave: the value, whether there is one,
/// whether it came from a secret source, and the error if the expression
/// is malformed. `secret` is meaningful whatever the outcome.
#[derive(Clone, Debug, Default)]
pub(crate) struct Resolution {
    pub value: String,
    pub ok: bool,
    pub secret: bool,
    pub err: Option<Error>,
}

impl Resolution {
    fn found(value: impl Into<String>) -> Resolution {
        Resolution {
            value: value.into(),
            ok: true,
            ..Resolution::default()
        }
    }

    fn lookup(value: Option<String>) -> Resolution {
        Resolution {
            ok: value.is_some(),
            value: value.unwrap_or_default(),
            ..Resolution::default()
        }
    }

    pub(crate) fn fail(err: Error) -> Resolution {
        Resolution {
            err: Some(err),
            ..Resolution::default()
        }
    }

    fn bad(msg: impl Into<String>) -> Resolution {
        Resolution::fail(Error::Other(msg.into()))
    }

    pub(crate) fn secret(mut self) -> Resolution {
        self.secret = true;
        self
    }

    /// The form the template renderer takes.
    pub(crate) fn into_result(self) -> Result<Option<String>, Error> {
        match self.err {
            Some(err) => Err(err),
            None => Ok(self.ok.then_some(self.value)),
        }
    }
}

/// A render error as the crate's error: `{{expr}}: <source>` for a
/// placeholder that could not be resolved.
pub(crate) fn render_error(err: RenderError<Error>) -> Error {
    match err {
        RenderError::Missing(me) => Error::Missing(me),
        RenderError::Resolve { expr, source, .. } => {
            Error::context(format!("{{{{{expr}}}}}"), source)
        }
    }
}

/// An instant with the abbreviation of its zone.
#[derive(Clone, Debug)]
pub(crate) struct Moment {
    pub time: Time,
    pub zone: String,
    /// Read from the system clock, in the machine's zone.
    local: bool,
}

fn env_source(file: &str, env: &str) -> String {
    if env.is_empty() {
        format!("{file} ($shared)")
    } else {
        format!("{file} [{env}]")
    }
}

impl Runner {
    /// Resolves a plain variable name (no `$`, no response reference) for
    /// a request, walking the precedence layers.
    pub(crate) fn lookup(
        &self,
        req: Option<&Arc<Request>>,
        name: &str,
        depth: usize,
    ) -> (VarInfo, bool, Option<Error>) {
        let mut info = VarInfo {
            name: name.to_string(),
            ..VarInfo::default()
        };
        let found = |info: &mut VarInfo, v: String, source: String, secret: bool| {
            info.value = v;
            info.source = source;
            info.secret = secret;
        };
        // --var and APIC_VAR_* are the documented way to inject a secret
        // in CI, and apic test already treats them as secret. Mark them so
        // here too, rather than leaving the two halves of the product
        // disagreeing about what a secret is.
        if let Some(v) = self.var(name) {
            found(&mut info, v, "--var".to_string(), true);
            return (info, true, None);
        }
        if let Ok(v) = std::env::var(format!("APIC_VAR_{name}")) {
            found(&mut info, v, format!("shell APIC_VAR_{name}"), true);
            return (info, true, None);
        }
        let captured = self
            .captured
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned();
        if let Some(v) = captured {
            found(&mut info, v, "captured this run".to_string(), true);
            return (info, true, None);
        }
        if let Some(v) = self.session_get(name) {
            found(&mut info, v, "session".to_string(), true);
            return (info, true, None);
        }
        let env = &self.opts.env;
        if let Some(v) = self.envs.private_vars(env).remove(name) {
            found(&mut info, v, env_source(apic_env::PRIVATE_FILE, env), true);
            return (info, true, None);
        }
        if let Some(v) = self.envs.public_vars(env).remove(name) {
            found(&mut info, v, env_source(apic_env::PUBLIC_FILE, env), false);
            return (info, true, None);
        }
        if let Some(v) = self.envs.dot_env.get(name) {
            found(&mut info, v.clone(), ".env".to_string(), true);
            return (info, true, None);
        }
        if let Some(req) = req {
            // Last declaration wins, like REST Client.
            for fv in req.file.vars.iter().rev() {
                if fv.name != name {
                    continue;
                }
                info.source = format!("{}:{} @{}", req.file.path, fv.line, fv.name);
                if depth > 8 {
                    let err = Error::Other(format!(
                        "variable {} exceeds max expansion depth",
                        quote(name)
                    ));
                    return (info, true, Some(err));
                }
                let mut secret = false;
                let rendered = apic_template::render(&fv.value, |e| {
                    let r = self.resolve_expr_meta(Some(req), e, depth + 1);
                    if r.secret {
                        secret = true;
                    }
                    r.into_result()
                });
                return match rendered {
                    Ok(v) => {
                        info.value = v;
                        info.secret = secret;
                        (info, true, None)
                    }
                    Err(err) => (info, false, Some(render_error(err))),
                };
            }
        }
        info.missing = true;
        info.source = "missing".to_string();
        if let Some(req) = req {
            if let Some(by) = self.project.captured_by(name) {
                if !Arc::ptr_eq(&by, req) {
                    info.captured_by = by.id();
                }
            }
        }
        (info, false, None)
    }

    /// Resolves any `{{expr}}` for a request.
    pub(crate) fn resolve_expr(
        &self,
        req: Option<&Arc<Request>>,
        expr: &str,
    ) -> Result<Option<String>, Error> {
        self.resolve_expr_meta(req, expr, 0).into_result()
    }

    pub(crate) fn resolve_expr_meta(
        &self,
        req: Option<&Arc<Request>>,
        expr: &str,
        depth: usize,
    ) -> Resolution {
        if expr.starts_with("$auth.") {
            return self.auth_builtin(req, expr);
        }
        if expr.starts_with('$') {
            return self.builtin(expr);
        }
        if expr.contains(".response.") {
            return match self.response_ref(expr) {
                Ok(v) => Resolution::lookup(v),
                Err(err) => Resolution::fail(err),
            };
        }
        let (info, ok, err) = self.lookup(req, expr, depth);
        Resolution {
            value: info.value,
            ok,
            secret: info.secret,
            err,
        }
    }

    /// Resolves a `$name ...` placeholder: apic's own built-ins, REST
    /// Client's (with its offset grammar) and JetBrains' `$random.*`
    /// family.
    pub(crate) fn builtin(&self, expr: &str) -> Resolution {
        if expr.starts_with("$random.") {
            return match random_builtin(expr) {
                Ok(v) => Resolution::found(v),
                Err(e) => Resolution::bad(e),
            };
        }
        let fields: Vec<&str> = expr.split_whitespace().collect();
        let (name, args) = (fields[0], &fields[1..]);
        match name {
            "$uuid" | "$guid" => return Resolution::found(uuid::Uuid::new_v4().to_string()),
            "$timestamp" => {
                return match offset_time(name, self.clock(), args) {
                    Ok(m) => Resolution::found(m.time.unix().to_string()),
                    Err(e) => Resolution::bad(e),
                }
            }
            "$isoTimestamp" => {
                return Resolution::found(format_time(self.clock().time.utc(), "UTC", RFC3339))
            }
            "$datetime" | "$localDatetime" => {
                let (layout, offset) = datetime_args(expr[name.len()..].trim());
                let mut m = match offset_time(name, self.clock(), &offset) {
                    Ok(m) => m,
                    Err(e) => return Resolution::bad(e),
                };
                // $localDatetime keeps the clock's own zone (the machine's,
                // or the test's); $datetime is always UTC.
                if name == "$datetime" {
                    m.time = m.time.utc();
                    m.zone = "UTC".to_string();
                }
                return Resolution::found(format_time(m.time, &m.zone, &layout));
            }
            "$projectRoot" => return Resolution::found(self.project.root.clone()),
            "$randomInt" => {
                let (mut lo, mut hi) = (0i64, 1000i64);
                if args.len() >= 2 {
                    match args[0].parse() {
                        Ok(n) => lo = n,
                        Err(_) => return Resolution::bad("$randomInt min must be an integer"),
                    }
                    match args[1].parse() {
                        Ok(n) => hi = n,
                        Err(_) => return Resolution::bad("$randomInt max must be an integer"),
                    }
                }
                if hi <= lo {
                    return Resolution::bad("$randomInt max must be greater than min");
                }
                // {{$randomInt}} is a convenience for sample payloads, not
                // a nonce or a token; nothing in apic derives a credential
                // from it.
                let span = hi.wrapping_sub(lo) as u64;
                return Resolution::found(lo.wrapping_add(random_below(span) as i64).to_string());
            }
            "$processEnv" => {
                if args.len() != 1 {
                    return Resolution::bad("$processEnv needs a variable name");
                }
                return Resolution::lookup(std::env::var(args[0]).ok());
            }
            "$dotenv" => {
                if args.len() != 1 {
                    return Resolution::bad("$dotenv needs a variable name");
                }
                return Resolution::lookup(self.envs.dot_env.get(args[0]).cloned()).secret();
            }
            _ => {}
        }
        if let Some(var) = name.strip_prefix("$env.") {
            return Resolution::lookup(std::env::var(var).ok());
        }
        Resolution::bad(format!("unknown built-in {name}"))
    }

    /// The time the built-ins read, with its zone.
    pub(crate) fn clock(&self) -> Moment {
        match &self.now {
            Some(now) => Moment {
                time: now(),
                zone: self.zone.clone().unwrap_or_default(),
                local: false,
            },
            None => {
                let now = Time::now();
                let (offset, zone) = LocalZone::system().lookup(now.unix());
                Moment {
                    time: now.with_offset(offset),
                    zone,
                    local: true,
                }
            }
        }
    }

    /// Resolves `<name>.response.<selector>` against a request already
    /// run in this invocation, e.g. login.response.body.$.token. One that
    /// has not run yet is missing rather than an error, so a `# @ref` to
    /// it runs it first, as for a captured variable, and the
    /// missing-variable error explains the rest.
    fn response_ref(&self, expr: &str) -> Result<Option<String>, Error> {
        let (name, sel) = expr.split_once(".response.").unwrap_or((expr, ""));
        let raw = {
            let results = self.results.lock().unwrap_or_else(|e| e.into_inner());
            results.get(name).and_then(|r| r.raw.clone())
        };
        let Some(raw) = raw else {
            return Ok(None);
        };
        apic_selector::select(&raw, sel).map_err(|e| Error::Other(e.to_string()))
    }
}

/// REST Client's offset units: `{{$timestamp -1 d}}`.
const OFFSET_UNITS: &str = "ms, s, m, h, d, w, M, Q or y";

/// Moves a moment by calendar years, months and days, keeping its time of
/// day; a day past the end of a month rolls into the next.
fn add_date(m: Moment, years: i64, months: i64, days: i64) -> Moment {
    let c = m.time.civil();
    let total = (c.year + years) * 12 + i64::from(c.month) - 1 + months;
    let (year, month) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
    let offset = m.time.offset_seconds();
    let at = |offset: i32| {
        Time::from_civil(
            year,
            month,
            1,
            c.hour,
            c.minute,
            c.second,
            c.nanosecond,
            offset,
        )
        .add_nanos((i128::from(c.day) - 1 + i128::from(days)) * 86_400 * 1_000_000_000)
    };
    let mut time = at(offset);
    if m.local {
        // The offset in force on the new date may differ (daylight time).
        let zone = LocalZone::system();
        let first = zone.lookup(time.unix()).0;
        time = at(first);
        let second = zone.lookup(time.unix()).0;
        if second != first {
            time = at(second);
        }
        let zone = zone.lookup(time.unix()).1;
        return Moment {
            time,
            zone,
            local: true,
        };
    }
    Moment { time, ..m }
}

/// Applies an optional `<n> <unit>` offset. Months, quarters and years
/// move by calendar, the rest by duration.
fn offset_time<S: AsRef<str>>(name: &str, m: Moment, args: &[S]) -> Result<Moment, String> {
    if args.is_empty() {
        return Ok(m);
    }
    if args.len() != 2 {
        let got: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
        return Err(format!(
            "{name}: an offset is `<n> <unit>` with the unit one of {OFFSET_UNITS}, got {}",
            quote(&got.join(" "))
        ));
    }
    let (num, unit) = (args[0].as_ref(), args[1].as_ref());
    let n: i64 = num
        .parse()
        .map_err(|_| format!("{name}: the offset {} is not an integer", quote(num)))?;
    let by = |nanos: i128| {
        let mut out = m.clone();
        out.time = m.time.add_nanos(i128::from(n) * nanos);
        if out.local {
            let (offset, zone) = LocalZone::system().lookup(out.time.unix());
            out.time = out.time.with_offset(offset);
            out.zone = zone;
        }
        out
    };
    Ok(match unit {
        "ms" => by(1_000_000),
        "s" => by(1_000_000_000),
        "m" => by(60_000_000_000),
        "h" => by(3_600_000_000_000),
        "d" => add_date(m, 0, 0, n),
        "w" => add_date(m, 0, 0, 7 * n),
        "M" => add_date(m, 0, n, 0),
        "Q" => add_date(m, 0, 3 * n, 0),
        "y" => add_date(m, n, 0, 0),
        _ => {
            return Err(format!(
                "{name}: unknown offset unit {}; use one of {OFFSET_UNITS}",
                quote(unit)
            ))
        }
    })
}

/// Reads what follows `$datetime` or `$localDatetime`: an optional format
/// (`rfc1123`, `iso8601` or a reference-time layout, quoted or not, spaces
/// included) and an optional trailing `<n> <unit>` offset. No format
/// means RFC 3339.
fn datetime_args(rest: &str) -> (String, Vec<String>) {
    let mut fields: Vec<&str> = rest.split_whitespace().collect();
    let mut offset = Vec::new();
    let n = fields.len();
    if n >= 2 && is_offset_unit(fields[n - 1]) {
        // offset_time checks the number.
        offset = fields[n - 2..].iter().map(|s| s.to_string()).collect();
        fields.truncate(n - 2);
    }
    let joined = fields.join(" ");
    let layout = match joined.trim_matches(['"', '\'']) {
        "" | "iso8601" => RFC3339,
        "rfc1123" => RFC1123,
        f => f,
    };
    (layout.to_string(), offset)
}

fn is_offset_unit(s: &str) -> bool {
    matches!(s, "ms" | "s" | "m" | "h" | "d" | "w" | "M" | "Q" | "y")
}

/// A random 64-bit number. The values are sample data for payloads, never
/// credentials.
fn random_u64() -> u64 {
    uuid::Uuid::new_v4().as_u128() as u64
}

/// A random number in `0..n`, evenly spread; `n` must not be zero.
fn random_below(n: u64) -> u64 {
    let zone = u64::MAX - u64::MAX % n;
    loop {
        let v = random_u64();
        if v < zone {
            return v % n;
        }
    }
}

static RE_RANDOM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\$random\.([a-z]+)(?:\((.*)\))?$").expect("a valid pattern"));

/// One `$random.<kind>(args)`: the kind, the trimmed arguments, and the
/// text between the parentheses for messages.
struct RandomCall<'a> {
    kind: &'a str,
    args: Vec<&'a str>,
    raw: &'a str,
}

const RANDOM_LETTERS: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// Resolves JetBrains' `$random.<kind>(args)`. The values are sample data
/// for payloads, never credentials, so the length defaults are small.
fn random_builtin(expr: &str) -> Result<String, String> {
    let Some(m) = RE_RANDOM.captures(expr.trim()) else {
        return Err(format!("{expr}: expected `$random.<kind>(args)`"));
    };
    let raw = m.get(2).map_or("", |m| m.as_str());
    let mut c = RandomCall {
        kind: m.get(1).map_or("", |m| m.as_str()),
        args: Vec::new(),
        raw,
    };
    if !raw.trim().is_empty() {
        c.args = raw.split(',').map(str::trim).collect();
    }
    match c.kind {
        "integer" => random_integer(&c),
        "float" => random_float(&c),
        "alphabetic" => random_string(&c, RANDOM_LETTERS),
        "alphanumeric" => random_string(&c, &format!("{RANDOM_LETTERS}0123456789")),
        "hexadecimal" => random_string(&c, "0123456789abcdef"),
        "email" => {
            if !c.args.is_empty() {
                return Err("$random.email takes no arguments".to_string());
            }
            Ok(format!("{}@example.com", random_pick(RANDOM_LETTERS, 8).to_lowercase()))
        }
        "uuid" => {
            if !c.args.is_empty() {
                return Err("$random.uuid takes no arguments".to_string());
            }
            Ok(uuid::Uuid::new_v4().to_string())
        }
        kind => Err(format!("unknown built-in $random.{kind}; the kinds are integer, float, alphabetic, alphanumeric, hexadecimal, email and uuid")),
    }
}

/// `n` characters drawn from `alphabet`.
fn random_pick(alphabet: &str, n: usize) -> String {
    let a = alphabet.as_bytes();
    (0..n)
        .map(|_| a[random_below(a.len() as u64) as usize] as char)
        .collect()
}

/// Renders `(length)` characters of `alphabet`, ten by default.
fn random_string(c: &RandomCall<'_>, alphabet: &str) -> Result<String, String> {
    let n = match c.args.as_slice() {
        [] => 10,
        [one] => one
            .parse::<usize>()
            .map_err(|_| format!("$random.{} takes one length, got ({})", c.kind, c.raw))?,
        _ => {
            return Err(format!(
                "$random.{} takes one length, got ({})",
                c.kind, c.raw
            ))
        }
    };
    Ok(random_pick(alphabet, n))
}

/// Renders `$random.integer(min, max)`, 0 to 1000 by default.
fn random_integer(c: &RandomCall<'_>) -> Result<String, String> {
    let (mut lo, mut hi) = (0i64, 1000i64);
    if !c.args.is_empty() {
        if c.args.len() != 2 {
            return Err(format!("$random.integer takes (min, max), got ({})", c.raw));
        }
        match (c.args[0].parse::<i64>(), c.args[1].parse::<i64>()) {
            (Ok(a), Ok(b)) if b > a => (lo, hi) = (a, b),
            _ => {
                return Err(format!(
                    "$random.integer needs two integers with max greater than min, got ({})",
                    c.raw
                ))
            }
        }
    }
    let span = hi.wrapping_sub(lo);
    if span <= 0 {
        // Overflowed.
        return Err(format!(
            "$random.integer: the range ({}) is too wide",
            c.raw
        ));
    }
    Ok((lo + random_below(span as u64) as i64).to_string())
}

/// Renders `$random.float(min, max)`, 0 to 1000 by default.
fn random_float(c: &RandomCall<'_>) -> Result<String, String> {
    let (mut lo, mut hi) = (0.0f64, 1000.0f64);
    if !c.args.is_empty() {
        if c.args.len() != 2 {
            return Err(format!("$random.float takes (min, max), got ({})", c.raw));
        }
        match (c.args[0].parse::<f64>(), c.args[1].parse::<f64>()) {
            (Ok(a), Ok(b)) if a.is_finite() && b.is_finite() && b > a => (lo, hi) = (a, b),
            _ => {
                return Err(format!(
                    "$random.float needs two finite numbers with max greater than min, got ({})",
                    c.raw
                ))
            }
        }
    }
    let unit = (random_u64() >> 11) as f64 / (1u64 << 53) as f64;
    Ok(format!("{:.3}", lo + unit * (hi - lo)))
}
