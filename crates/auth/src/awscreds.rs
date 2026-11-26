use std::collections::BTreeMap;
use std::path::Path;

use apic_gocompat::quote;
use apic_httpc::CancelToken;
use serde_json::Value;

use crate::apply::Env;
use crate::command;
use crate::error::Error;
use crate::sigv4::AwsCredentials;
use crate::spec::Spec;

/// What the aws auth type needs to sign a request.
#[derive(Clone, Debug, Default)]
pub(crate) struct AwsSettings {
    pub creds: AwsCredentials,
    pub region: String,
    /// Where the credentials came from, for error messages and describe.
    #[cfg_attr(not(test), allow(dead_code))]
    pub source: String,
}

type Section = BTreeMap<String, String>;
type Ini = BTreeMap<String, Section>;

/// Finds credentials and a region the way the AWS CLI does, without the
/// SDK:
///
///  1. AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY / AWS_SESSION_TOKEN (unless profile= is set)
///  2. static keys in ~/.aws/credentials or ~/.aws/config for the profile
///  3. `aws configure export-credentials --profile <p>` when the AWS CLI v2 is installed,
///     which resolves SSO, assumed roles, credential_process and the rest
///
/// Region: region= option, AWS_REGION, AWS_DEFAULT_REGION, then the profile's region.
pub(crate) fn resolve_aws(cancel: &CancelToken, s: &Spec, env: &Env) -> Result<AwsSettings, Error> {
    let mut profile = s.option("profile").to_string();
    let explicit_profile = !profile.is_empty();
    if profile.is_empty() {
        profile = env.getenv("AWS_PROFILE");
    }
    if profile.is_empty() {
        profile = "default".to_string();
    }
    let mut out = AwsSettings {
        region: s.option("region").to_string(),
        ..AwsSettings::default()
    };
    if out.region.is_empty() {
        out.region = first_non_empty(&[env.getenv("AWS_REGION"), env.getenv("AWS_DEFAULT_REGION")]);
    }

    let home = home_dir(env);
    let aws_file = |name: &str| {
        Path::new(&home)
            .join(".aws")
            .join(name)
            .to_string_lossy()
            .into_owned()
    };
    let cred_file = read_ini(&first_non_empty(&[
        env.getenv("AWS_SHARED_CREDENTIALS_FILE"),
        aws_file("credentials"),
    ]));
    let conf_file = read_ini(&first_non_empty(&[
        env.getenv("AWS_CONFIG_FILE"),
        aws_file("config"),
    ]));
    let empty = Section::new();
    let mut conf_section = conf_file
        .get(&format!("profile {profile}"))
        .cloned()
        .unwrap_or_default();
    if profile == "default" {
        conf_section = merge_ini(conf_file.get("default").unwrap_or(&empty), &conf_section);
    }
    if out.region.is_empty() {
        out.region = conf_section.get("region").cloned().unwrap_or_default();
    }

    let id = env.getenv("AWS_ACCESS_KEY_ID");
    let secret = env.getenv("AWS_SECRET_ACCESS_KEY");
    if !explicit_profile && !id.is_empty() && !secret.is_empty() {
        out.creds = AwsCredentials {
            access_key_id: id,
            secret_access_key: secret,
            session_token: env.getenv("AWS_SESSION_TOKEN"),
        };
        out.source = "environment".to_string();
        out.check_region()?;
        return Ok(out);
    }
    for sec in [cred_file.get(&profile).unwrap_or(&empty), &conf_section] {
        let value = |k: &str| sec.get(k).cloned().unwrap_or_default();
        if !value("aws_access_key_id").is_empty() && !value("aws_secret_access_key").is_empty() {
            out.creds = AwsCredentials {
                access_key_id: value("aws_access_key_id"),
                secret_access_key: value("aws_secret_access_key"),
                session_token: value("aws_session_token"),
            };
            out.source = format!("profile {profile}");
            out.check_region()?;
            return Ok(out);
        }
    }
    let search_path = env.getenv("PATH");
    if let Ok(cli) = command::look_path("aws", &search_path) {
        out.creds = export_credentials(cancel, &search_path, &cli, &profile)?;
        out.source = format!("aws cli, profile {profile}");
        out.check_region()?;
        return Ok(out);
    }
    Err(Error::msg(format!(
        "aws: no credentials for profile {}: set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY, add the profile to ~/.aws/credentials, or install the AWS CLI so SSO and assumed-role profiles can be used",
        quote(&profile)
    )))
}

impl AwsSettings {
    fn check_region(&self) -> Result<(), Error> {
        if self.region.is_empty() {
            return Err(Error::msg(
                "aws: no region: add region=.. to @auth aws, set AWS_REGION, or put region in the profile",
            ));
        }
        Ok(())
    }
}

/// Asks the AWS CLI for the profile's current credentials.
fn export_credentials(
    cancel: &CancelToken,
    search_path: &str,
    cli: &str,
    profile: &str,
) -> Result<AwsCredentials, Error> {
    let args = [
        "configure",
        "export-credentials",
        "--profile",
        profile,
        "--format",
        "process",
    ]
    .map(String::from);
    let raw = command::output(cancel, search_path, cli, &args).map_err(|f| {
        if f.cancelled {
            return Error::Cancelled;
        }
        let msg = match f.stderr.trim() {
            "" => f.message.as_str(),
            msg => msg,
        };
        Error::msg(format!(
            "aws: `aws configure export-credentials --profile {profile}` failed: {msg}"
        ))
    })?;
    let unexpected = || {
        Error::msg(
            "aws: unexpected output from `aws configure export-credentials` (needs AWS CLI v2)",
        )
    };
    let v: Value = serde_json::from_slice(&raw).map_err(|_| unexpected())?;
    let field = |name: &str| match v.get(name) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(unexpected()),
    };
    let creds = AwsCredentials {
        access_key_id: field("AccessKeyId")?,
        secret_access_key: field("SecretAccessKey")?,
        session_token: field("SessionToken")?,
    };
    if creds.access_key_id.is_empty() {
        return Err(unexpected());
    }
    Ok(creds)
}

/// Parses the subset of INI used by AWS config files: [sections] and
/// key = value lines. Missing files yield an empty map.
fn read_ini(path: &str) -> Ini {
    let mut out = Ini::new();
    let Ok(data) = std::fs::read(path) else {
        return out;
    };
    let text = String::from_utf8_lossy(&data);
    let mut section = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_string();
            out.entry(section.clone()).or_default();
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            if section.is_empty() || raw.starts_with([' ', '\t']) {
                continue; // nested keys (e.g. s3 = ...) are not needed
            }
            out.entry(section.clone())
                .or_default()
                .insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    out
}

fn merge_ini(base: &Section, over: &Section) -> Section {
    let mut out = base.clone();
    out.extend(over.iter().map(|(k, v)| (k.clone(), v.clone())));
    out
}

fn first_non_empty(vals: &[String]) -> String {
    vals.iter()
        .find(|v| !v.is_empty())
        .cloned()
        .unwrap_or_default()
}

/// The user's home directory, empty when the environment does not say.
fn home_dir(env: &Env) -> String {
    env.getenv(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
}
