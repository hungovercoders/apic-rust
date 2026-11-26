use std::collections::BTreeMap;

use apic_gocompat::Time;
use apic_httpc::Request;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::apply::hex;
use crate::urlutil::{parse_query, split_url};

/// Static AWS credentials.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AwsCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: String,
}

/// Signs `req` in place with AWS Signature Version 4 (header auth). `body`
/// is the request body as sent. Behaviour matches the AWS SDK signer: the
/// path is URI-encoded twice except for S3, query parameters are sorted,
/// and Content-Length is signed when the request carries a body.
pub fn sign_sig_v4(
    req: &mut Request,
    body: &[u8],
    creds: &AwsCredentials,
    service: &str,
    region: &str,
    at: Time,
) {
    let payload_hash = sha256_hex(body);
    req.headers.set("X-Amz-Content-Sha256", &payload_hash);
    sign(req, &payload_hash, creds, service, region, at);
}

/// Signs without adding X-Amz-Content-Sha256 (used by tests against the
/// official vectors, which omit that header).
#[cfg(test)]
pub(crate) fn sign_without(
    req: &mut Request,
    creds: &AwsCredentials,
    service: &str,
    region: &str,
    at: Time,
) {
    sign(req, &sha256_hex(b""), creds, service, region, at);
}

fn sign(
    req: &mut Request,
    payload_hash: &str,
    creds: &AwsCredentials,
    service: &str,
    region: &str,
    at: Time,
) {
    let c = at.utc().civil();
    let date_stamp = format!("{:04}{:02}{:02}", c.year, c.month, c.day);
    let amz_date = format!("{date_stamp}T{:02}{:02}{:02}Z", c.hour, c.minute, c.second);

    req.headers.set("X-Amz-Date", &amz_date);
    if !creds.session_token.is_empty() {
        req.headers
            .set("X-Amz-Security-Token", &creds.session_token);
    }

    // The host as it is sent: an override, or the URL's in lower case.
    let url = split_url(&req.url);
    let host = match (&req.host, req.headers.get("Host")) {
        (Some(host), _) if !host.is_empty() => host.clone(),
        (_, Some(host)) if !host.is_empty() => host.to_string(),
        _ => url.host.to_ascii_lowercase(),
    };
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    for (name, values) in req.headers.to_map() {
        let lower = name.to_lowercase();
        if lower == "host" || IGNORED_HEADERS.contains(&lower.as_str()) {
            continue;
        }
        headers.insert(lower, canonical_value(&values));
    }
    headers.insert("host".to_string(), host);
    if !req.body.is_empty() {
        headers.insert("content-length".to_string(), req.body.len().to_string());
    }
    let mut canon_headers = String::new();
    for (n, v) in &headers {
        canon_headers.push_str(&format!("{n}:{v}\n"));
    }
    let signed_headers = headers.keys().cloned().collect::<Vec<_>>().join(";");

    let canonical_request = [
        req.method.as_str(),
        &canonical_path(&url.wire_path(), service),
        &canonical_query(url.query.unwrap_or("")),
        &canon_headers,
        &signed_headers,
        payload_hash,
    ]
    .join("\n");

    let scope = [date_stamp.as_str(), region, service, "aws4_request"].join("/");
    let string_to_sign = [
        "AWS4-HMAC-SHA256",
        &amz_date,
        &scope,
        &sha256_hex(canonical_request.as_bytes()),
    ]
    .join("\n");

    let k_date = hmac_sha256(
        format!("AWS4{}", creds.secret_access_key).as_bytes(),
        &date_stamp,
    );
    let k_region = hmac_sha256(&k_date, region);
    let k_service = hmac_sha256(&k_region, service);
    let k_signing = hmac_sha256(&k_service, "aws4_request");
    let signature = hex(&hmac_sha256(&k_signing, &string_to_sign));

    req.headers.set(
        "Authorization",
        &format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            creds.access_key_id, scope, signed_headers, signature
        ),
    );
}

const IGNORED_HEADERS: [&str; 5] = [
    "authorization",
    "user-agent",
    "x-amzn-trace-id",
    "expect",
    "transfer-encoding",
];

fn canonical_value(values: &[String]) -> String {
    values
        .iter()
        .map(|v| v.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join(",")
}

/// The path as the service sees it: the path on the wire, encoded once
/// more except for S3.
fn canonical_path(path: &str, service: &str) -> String {
    if service == "s3" {
        return path.to_string();
    }
    aws_escape(path.as_bytes(), false)
}

fn canonical_query(raw: &str) -> String {
    let mut q: BTreeMap<Vec<u8>, Vec<Vec<u8>>> = BTreeMap::new();
    for (k, v) in parse_query(raw) {
        q.entry(k).or_default().push(v);
    }
    q.remove(b"X-Amz-Signature".as_slice());
    let mut parts = Vec::new();
    for (k, mut vals) in q {
        vals.sort();
        for v in vals {
            parts.push(format!("{}={}", aws_escape(&k, true), aws_escape(&v, true)));
        }
    }
    parts.join("&")
}

/// Percent-encodes every byte except the unreserved set and, unless
/// `encode_slash` is set, the path separator.
fn aws_escape(s: &[u8], encode_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for &c in s {
        match c {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(c as char)
            }
            b'/' if !encode_slash => out.push('/'),
            _ => out.push_str(&format!("%{c:02X}")),
        }
    }
    out
}

fn sha256_hex(b: &[u8]) -> String {
    hex(&Sha256::digest(b))
}

fn hmac_sha256(key: &[u8], data: &str) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts a key of any length");
    mac.update(data.as_bytes());
    mac.finalize().into_bytes().to_vec()
}
