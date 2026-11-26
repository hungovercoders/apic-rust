use std::path::{Path, PathBuf};

use apic_httpd::{testcerts, Request as HttpRequest, Response as HttpResponse, TestServer};

use super::*;
use crate::tls::{host_override, TlsSettings};
use crate::{EXIT_TRANSPORT, EXIT_USAGE};

/// The test certificate authority with its client certificate and a
/// stranger's, written as PEM under dir/certs.
struct Pki {
    ca_file: String,
    client_cert: String,
    client_key: String,
    stranger_cert: String,
    stranger_key: String,
}

fn new_pki(dir: &Path) -> Pki {
    let certs = dir.join("certs");
    std::fs::create_dir_all(&certs).unwrap();
    let write = |name: &str, data: &str, mode: u32| -> String {
        let path: PathBuf = certs.join(name);
        std::fs::write(&path, data).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        #[cfg(not(unix))]
        let _ = mode;
        path.to_str().unwrap().to_string()
    };
    Pki {
        ca_file: write("ca.pem", testcerts::CA_PEM, 0o644),
        client_cert: write("client.pem", testcerts::CLIENT_CERT_PEM, 0o644),
        client_key: write("client-key.pem", testcerts::CLIENT_KEY_PEM, 0o600),
        stranger_cert: write("stranger.pem", testcerts::STRANGER_CERT_PEM, 0o644),
        stranger_key: write("stranger-key.pem", testcerts::STRANGER_KEY_PEM, 0o600),
    }
}

/// Requires a client certificate signed by the test authority.
fn mtls_server() -> TestServer {
    TestServer::builder()
        .require_client_cert()
        .start(|r: &HttpRequest| {
            let user = r
                .tls
                .as_ref()
                .and_then(|t| t.peer_common_name())
                .unwrap_or_default();
            HttpResponse::json(200, format!(r#"{{"user":"{user}"}}"#))
        })
}

#[test]
fn client_certificates_and_private_ca() {
    let tmp = write_project(&[
        (
            "api.http",
            "### me\n# @name me\n# @assert status == 200\n# @assert body.$.user == alice\nGET {{baseUrl}}/me\n",
        ),
        ("http-client.env.json", r#"{"dev": {}}"#),
    ]);
    let dir = tmp.path();
    let p = new_pki(dir);
    let srv = mtls_server();
    std::fs::write(
        dir.join("http-client.env.json"),
        format!(r#"{{"dev": {{"baseUrl": "{}"}}}}"#, srv.url()),
    )
    .unwrap();
    let dev = |mut opts: Options| {
        opts.env = "dev".into();
        opts.no_session = true;
        opts
    };
    let run = |opts: Options| {
        let r = new_runner(dir, dev(opts));
        let (res, err) = r.run(&ctx(), &r.project.requests()[0]);
        (res, err)
    };
    let exit_of = |opts: Options| exit(run(opts).1.as_ref());
    let flags = |ca: &str, cert: &str, key: &str| Options {
        ca_cert: ca.into(),
        cert: cert.into(),
        key: key.into(),
        ..Options::default()
    };

    // Nothing configured: the private CA is unknown, so the handshake
    // fails as a transport error.
    let (_, err) = run(Options::default());
    assert_eq!(exit(err.as_ref()), EXIT_TRANSPORT, "without a CA: {err:?}");
    assert_eq!(crate::code_of(err.as_ref().unwrap()), crate::Code::Tls);
    // --insecure skips verification, but the server still wants a client
    // cert.
    assert_eq!(
        exit_of(Options {
            insecure: true,
            ..Options::default()
        }),
        EXIT_TRANSPORT,
        "insecure without a cert"
    );
    // Flags: CA trusted, certificate presented.
    let (res, err) = run(flags(&p.ca_file, &p.client_cert, &p.client_key));
    let res = res.unwrap_or_else(|| panic!("with flags: {err:?}"));
    assert!(res.ok, "with flags: {res:?}");
    let tls = res.request.tls.as_ref().expect("tls info");
    assert!(
        tls.cert_file == p.client_cert && tls.ca_file == p.ca_file && !tls.insecure,
        "tls info = {tls:?}"
    );
    // A certificate the server does not trust is a transport error too.
    assert_eq!(
        exit_of(flags(&p.ca_file, &p.stranger_cert, &p.stranger_key)),
        EXIT_TRANSPORT,
        "stranger cert"
    );

    // apic.yaml, with project-relative paths.
    let yaml = "tls:\n  caFile: certs/ca.pem\n  certFile: certs/client.pem\n  keyFile: certs/client-key.pem\n";
    std::fs::write(dir.join("apic.yaml"), yaml).unwrap();
    let (res, err) = run(Options::default());
    let res = res.unwrap_or_else(|| panic!("with apic.yaml: {err:?}"));
    assert!(res.ok, "with apic.yaml: {res:?}");
    let tls = res.request.tls.as_ref().unwrap();
    assert!(
        tls.cert_file == "certs/client.pem" && tls.key_file == "certs/client-key.pem",
        "tls info = {tls:?}"
    );
    let r = new_runner(dir, dev(Options::default()));
    let d = r.describe(&r.project.requests()[0]);
    assert_eq!(
        d.tls.expect("describe tls").to_string(),
        "client cert certs/client.pem (key certs/client-key.pem) · ca certs/ca.pem"
    );

    // A per-host override wins over the defaults: the stranger for this
    // host.
    let host_yaml = format!("{yaml}  hosts:\n    127.0.0.1: {{certFile: certs/stranger.pem, keyFile: certs/stranger-key.pem}}\n    '*.example.com': {{verifyHost: false}}\n");
    std::fs::write(dir.join("apic.yaml"), host_yaml).unwrap();
    assert_eq!(
        exit_of(Options::default()),
        EXIT_TRANSPORT,
        "host override should present the stranger"
    );
    let r = new_runner(dir, dev(Options::default()));
    let hosts = &r.project.config.tls.hosts;
    assert!(
        host_override(hosts, "API.example.com:8443").is_some(),
        "wildcard host override should match a subdomain with a port"
    );
    assert!(
        host_override(hosts, "example.com").is_none(),
        "*.example.com must not match the apex"
    );

    // JetBrains: the env file's SSLConfiguration block.
    std::fs::remove_file(dir.join("apic.yaml")).unwrap();
    let private = dir.join("http-client.private.env.json");
    std::fs::write(
        &private,
        r#"{"dev": {"SSLConfiguration": {"clientCertificate": {"path": "certs/client.pem", "keyPath": "certs/client-key.pem"}, "verifyHostCertificate": false}}}"#,
    )
    .unwrap();
    let (res, err) = run(Options::default());
    let res = res.unwrap_or_else(|| panic!("JetBrains block: {err:?}"));
    let tls = res.request.tls.as_ref().unwrap();
    assert!(
        res.ok && tls.insecure && tls.cert_file == "certs/client.pem",
        "JetBrains block: {res:?}"
    );
    let r = new_runner(dir, dev(Options::default()));
    let d = r.describe(&r.project.requests()[0]);
    assert_eq!(
        d.variables.len(),
        1,
        "SSLConfiguration must not be a variable: {:?}",
        d.variables
    );
    // A key with a passphrase is refused up front.
    std::fs::write(
        &private,
        r#"{"dev": {"SSLConfiguration": {"clientCertificate": "certs/client.pem", "hasCertificatePassphrase": true}}}"#,
    )
    .unwrap();
    let passphrase = |err: Option<Error>, what: &str| {
        let err = err.unwrap_or_else(|| panic!("{what}: no error"));
        assert!(
            exit(Some(&err)) == EXIT_USAGE && err.to_string().contains("passphrase"),
            "{what}: want a usage error, got {err}"
        );
    };
    passphrase(run(flags(&p.ca_file, "", "")).1, "passphrase");
    std::fs::remove_file(&private).unwrap();

    // Configuration mistakes are usage errors, not network ones.
    let api = dir.join("api.http");
    let (_, err) = run(flags(api.to_str().unwrap(), "", ""));
    let err = err.expect("bad CA file");
    assert!(
        exit(Some(&err)) == EXIT_USAGE && err.to_string().contains("no PEM certificates"),
        "bad CA file: {err}"
    );
    assert_eq!(crate::code_of(&err), crate::Code::TlsConfig);
    let missing = dir.join("certs").join("missing.pem");
    assert_eq!(
        exit_of(flags(&p.ca_file, missing.to_str().unwrap(), "")),
        EXIT_USAGE,
        "missing cert"
    );
    // A passphrase key reached after the same files were cached from
    // elsewhere is still refused: the check comes before the cache.
    std::fs::write(dir.join("apic.yaml"), yaml).unwrap();
    let r = new_runner(dir, dev(Options::default()));
    r.tls_config(&r.tls_for(""))
        .expect("the project's settings load");
    let settings = |ca: &str| TlsSettings {
        ca_file: ca.into(),
        verify: true,
        ..TlsSettings::default()
    };
    passphrase(
        r.tls_config(&TlsSettings {
            cert_file: "certs/client.pem".into(),
            key_file: "certs/client-key.pem".into(),
            passphrase: true,
            ..settings("certs/ca.pem")
        })
        .err(),
        "passphrase after cache",
    );
    // An absolute path in apic.yaml or an env file is confined like a
    // relative one; only a flag path is the user's own.
    r.tls_config(&settings(&p.ca_file))
        .expect("absolute config path inside the project");
    let elsewhere = tempfile::tempdir().unwrap();
    let outside = elsewhere.path().join("ca.pem");
    std::fs::write(&outside, testcerts::CA_PEM).unwrap();
    let outside = outside.to_str().unwrap();
    let err = r
        .tls_config(&settings(outside))
        .expect_err("outside the project");
    assert!(
        exit(Some(&err)) == EXIT_USAGE && err.to_string().contains("outside the project root"),
        "absolute config path outside the project: {err}"
    );
    r.tls_config(&TlsSettings {
        ca_from_flag: true,
        ..settings(outside)
    })
    .expect("the same path from a flag");
    std::fs::write(dir.join("apic.yaml"), "tls:\n  caFile: ../outside.pem\n").unwrap();
    let (_, err) = run(Options::default());
    let err = err.expect("escaping path");
    assert!(
        exit(Some(&err)) == EXIT_USAGE && err.to_string().contains("outside the project root"),
        "escaping path: {err}"
    );
    std::fs::remove_file(dir.join("apic.yaml")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p.client_key, std::fs::Permissions::from_mode(0o644)).unwrap();
        let (_, err) = run(flags(&p.ca_file, &p.client_cert, &p.client_key));
        let err = err.expect("world-readable key");
        assert!(
            exit(Some(&err)) == EXIT_USAGE
                && err.to_string().contains("world-readable (mode 0644)"),
            "world-readable key: {err}"
        );
    }
}
