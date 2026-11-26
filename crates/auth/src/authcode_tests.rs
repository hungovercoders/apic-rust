use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use apic_gocompat::Time;
use apic_httpc::{CancelToken, Client, Request};
use apic_httpd::{Handler, Mux, Response, TestServer};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::apply::{apply, hash_key, Cache, Env};
use crate::auth_tests::{authz, clock, get, prompt, text, MemCache};
use crate::authcode::{pkce, redirect_target};
use crate::oauth2::{cache_key, describe_cached};
use crate::spec::parse;
use crate::urlutil::{parse_query, split_url};
use crate::{Error, ERR_NOT_INTERACTIVE};

fn json_response(status: u16, value: serde_json::Value) -> Response {
    Response::json(status, value.to_string())
}

/// A parameter of a URL's query.
fn query_param(url: &str, name: &str) -> String {
    parse_query(split_url(url).query.unwrap_or(""))
        .into_iter()
        .find(|(k, _)| k == name.as_bytes())
        .map(|(_, v)| String::from_utf8_lossy(&v).into_owned())
        .unwrap_or_default()
}

/// Plays an authorization server: /authorize checks the PKCE challenge and
/// redirects the "browser" back with a code, /token checks the verifier
/// against it and issues a token with a refresh token.
fn auth_server() -> (TestServer, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let authorizes = Arc::new(AtomicUsize::new(0));
    let tokens = Arc::new(AtomicUsize::new(0));
    let challenges: Arc<Mutex<HashMap<String, String>>> = Arc::default(); // code -> challenge
    let mut mux: Mux<Handler> = Mux::new();
    let (count, issued) = (authorizes.clone(), challenges.clone());
    mux.handle_fn("/authorize", move |r| {
        count.fetch_add(1, Ordering::SeqCst);
        let q = |name: &str| r.query_get(name).unwrap_or_default();
        if q("response_type") != "code"
            || q("client_id") != "cid"
            || q("code_challenge_method") != "S256"
            || q("code_challenge").is_empty()
            || q("state").is_empty()
            || q("scope") != "openid profile"
        {
            return Response::error(400, &format!("bad authorize request: {}", r.raw_query));
        }
        let redirect = q("redirect_uri");
        if !redirect.starts_with("http://127.0.0.1:") || !redirect.ends_with("/callback") {
            return Response::error(400, "bad redirect_uri");
        }
        let code = format!("code-{}", &q("state")[..4]);
        issued
            .lock()
            .unwrap()
            .insert(code.clone(), q("code_challenge"));
        Response::redirect(
            r,
            &format!("{}?code={}&state={}", redirect, code, q("state")),
            302,
        )
    })
    .unwrap();
    let (count, issued) = (tokens.clone(), challenges);
    mux.handle_fn("/token", move |r| {
        count.fetch_add(1, Ordering::SeqCst);
        match r.form_value("grant_type").as_str() {
            "authorization_code" => {
                let sum = Sha256::digest(r.form_value("code_verifier").as_bytes());
                let challenge = issued.lock().unwrap().get(&r.form_value("code")).cloned();
                if challenge != Some(URL_SAFE_NO_PAD.encode(sum))
                    || r.form_value("client_id") != "cid"
                    || !r
                        .form_value("redirect_uri")
                        .starts_with("http://127.0.0.1:")
                {
                    return json_response(
                        400,
                        json!({"error": "invalid_grant", "error_description": "verifier or code"}),
                    );
                }
                json_response(
                    200,
                    json!({"access_token": "at-1", "refresh_token": "rt-1", "expires_in": 3600}),
                )
            }
            "refresh_token" => {
                if r.form_value("refresh_token") != "rt-1" {
                    return json_response(400, json!({"error": "invalid_grant"}));
                }
                json_response(200, json!({"access_token": "at-2", "expires_in": 3600}))
            }
            _ => json_response(400, json!({"error": "unsupported_grant_type"})),
        }
    })
    .unwrap();
    (TestServer::new(mux.into_handler()), authorizes, tokens)
}

type Opener = Option<crate::BrowserOpener>;

/// Visits the authorization URL the way a browser would, following the
/// redirect back to apic's loopback listener.
fn browser() -> Opener {
    Some(Box::new(|u| {
        let resp = Client::new()
            .execute(&Request::get(u))
            .map_err(|e| e.to_string())?;
        assert_eq!(
            resp.status_code,
            200,
            "browser ended on {} at {}: {}",
            resp.status_code,
            resp.url,
            resp.text()
        );
        assert_eq!(
            resp.text(),
            "<!doctype html><title>apic</title><p>Signed in. You can close this tab and go back to the terminal.</p>"
        );
        Ok(())
    }))
}

fn no_browser(why: &'static str) -> Opener {
    Some(Box::new(move |_| panic!("{why}")))
}

fn bg() -> CancelToken {
    CancelToken::new()
}

#[test]
fn authorization_code_round_trip_cache_and_refresh() {
    let (srv, authorizes, tokens) = auth_server();
    let s = parse(&format!(
        r#"oauth2 grant=authorization_code authUrl={0}/authorize tokenUrl={0}/token clientId=cid scope="openid profile""#,
        srv.url()
    ))
    .unwrap();
    let cache = Arc::new(MemCache::default());
    let (buf, stderr) = prompt();
    let start = Time::now();
    let (now, read_now) = clock(start);
    let mut env = Env {
        cache: Some(cache.clone()),
        stderr,
        interactive: true,
        open_browser: browser(),
        now: read_now,
        ..Env::default()
    };

    let mut req = get("http://x/");
    apply(&bg(), &s, &mut req, b"", &env).unwrap();
    assert_eq!(authz(&req), "Bearer at-1");
    assert_eq!(authorizes.load(Ordering::SeqCst), 1);
    assert_eq!(tokens.load(Ordering::SeqCst), 1);
    let shown = text(&buf);
    assert!(
        shown.starts_with(&format!("\nTo sign in, open {}/authorize?client_id=cid&code_challenge=", srv.url()))
            && shown.contains("\n(the redirect URI is http://127.0.0.1:")
            && shown.ends_with("/callback; register it with the provider if it asks)\nWaiting for the browser...\n"),
        "prompt: {shown:?}"
    );
    // Cached: no browser, no token request.
    let mut req = get("http://x/");
    env.open_browser = no_browser("browser opened with a cached token");
    env.interactive = false;
    apply(&bg(), &s, &mut req, b"", &env).unwrap();
    assert_eq!(authz(&req), "Bearer at-1");
    // Expired: refreshed with the refresh token, still no browser, even
    // when nobody is at the terminal.
    let later = start + Duration::from_secs(2 * 3600);
    *now.lock().unwrap() = later;
    let mut req = get("http://x/");
    apply(&bg(), &s, &mut req, b"", &env).unwrap();
    assert_eq!(authz(&req), "Bearer at-2");
    assert_eq!(authorizes.load(Ordering::SeqCst), 1);
    assert_eq!(tokens.load(Ordering::SeqCst), 2);
    // The cache entry is described, never printed.
    for (k, v) in cache.0.lock().unwrap().iter() {
        assert!(k.starts_with("$oauth2:"), "cache key {k}");
        assert_eq!(describe_cached(v, later), "token, expires in 1h0m0s");
    }
}

#[test]
fn authorization_code_refuses_without_a_terminal() {
    let (srv, authorizes, _) = auth_server();
    let s = parse(&format!(
        "oauth2 grant=authorization_code authUrl={0}/authorize tokenUrl={0}/token clientId=cid",
        srv.url()
    ))
    .unwrap();
    let mut req = get("http://x/");
    let env = Env {
        cache: Some(Arc::new(MemCache::default())),
        open_browser: no_browser("browser opened"),
        ..Env::default()
    };
    let err = apply(&bg(), &s, &mut req, b"", &env).unwrap_err();
    assert_eq!(err, Error::NotInteractive);
    assert_eq!(err.to_string(), ERR_NOT_INTERACTIVE);
    assert!(err.to_string().contains("interactively"));
    assert_eq!(authorizes.load(Ordering::SeqCst), 0);
    assert_eq!(authz(&req), "", "nothing should have been sent");
}

#[test]
fn authorization_code_rejects_wrong_state() {
    let (srv, _, tokens) = auth_server();
    let s = parse(&format!(
        "oauth2 grant=authorization_code authUrl={0}/authorize tokenUrl={0}/token clientId=cid",
        srv.url()
    ))
    .unwrap();
    // A "browser" that comes back with somebody else's state.
    let forged: Opener = Some(Box::new(|u| {
        let cb = format!(
            "{}?code=stolen&state=not-ours",
            query_param(u, "redirect_uri")
        );
        let resp = Client::new()
            .send(&Request::get(&cb))
            .map_err(|e| e.to_string())?;
        assert_eq!(resp.status_code, 400, "forged callback");
        assert_eq!(
            resp.text(),
            "<!doctype html><title>apic</title><p>Sign-in failed: oauth2: the redirect carried the wrong state; someone else may have sent it, so the sign-in was refused</p>"
        );
        Ok(())
    }));
    let mut req = get("http://x/");
    let (_, stderr) = prompt();
    let env = Env {
        interactive: true,
        open_browser: forged,
        stderr,
        ..Env::default()
    };
    let err = apply(&bg(), &s, &mut req, b"", &env).unwrap_err();
    assert!(err.to_string().contains("wrong state"), "{err}");
    assert_eq!(tokens.load(Ordering::SeqCst), 0);
    // And the provider saying no.
    let refused: Opener = Some(Box::new(|u| {
        let cb = format!(
            "{}?error=access_denied&error_description=nope&state={}",
            query_param(u, "redirect_uri"),
            query_param(u, "state")
        );
        Client::new()
            .send(&Request::get(&cb))
            .map_err(|e| e.to_string())?;
        Ok(())
    }));
    let (_, stderr) = prompt();
    let env = Env {
        interactive: true,
        open_browser: refused,
        stderr,
        ..Env::default()
    };
    let err = apply(&bg(), &s, &mut req, b"", &env).unwrap_err();
    assert_eq!(
        err.to_string(),
        "oauth2: the provider refused: access_denied nope"
    );
}

// A redirectUrl is listened on and sent to the provider as written, and an
// authUrl keeps the query it came with.
#[test]
fn authorization_code_redirect_url_and_auth_url_query() {
    let port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    let seen: Arc<Mutex<String>> = Arc::default();
    let record = seen.clone();
    let srv = TestServer::new(move |r: &apic_httpd::Request| {
        if r.path == "/token" {
            *record.lock().unwrap() = r.form_value("redirect_uri");
            return json_response(200, json!({"access_token": "at", "id_token": "idt"}));
        }
        Response::not_found()
    });
    let s = parse(&format!(
        "oauth2 grant=authorization_code authUrl={0}/authorize?tenant=t1&state=theirs tokenUrl={0}/token clientId=cid redirectUrl=http://localhost:{1}/signed/in audience=api",
        srv.url(),
        port
    ))
    .unwrap();
    let auth_url: Arc<Mutex<String>> = Arc::default();
    let opened = auth_url.clone();
    let env = Env {
        interactive: true,
        open_browser: Some(Box::new(move |u| {
            *opened.lock().unwrap() = u.to_string();
            let cb = format!(
                "http://127.0.0.1:{}/signed/in?code=c1&state={}",
                port,
                query_param(u, "state")
            );
            let resp = Client::new()
                .send(&Request::get(&cb))
                .map_err(|e| e.to_string())?;
            assert_eq!(resp.status_code, 200);
            Ok(())
        })),
        ..Env::default()
    };
    let toks = crate::oauth2_tokens(&bg(), &s, &env, true).unwrap();
    assert_eq!((toks.access.as_str(), toks.id.as_str()), ("at", "idt"));
    let want = format!("http://localhost:{port}/signed/in");
    assert_eq!(*seen.lock().unwrap(), want);
    let u = auth_url.lock().unwrap().clone();
    assert!(
        u.starts_with(&format!(
            "{}/authorize?audience=api&client_id=cid&code_challenge=",
            srv.url()
        )),
        "{u}"
    );
    assert_eq!(query_param(&u, "tenant"), "t1");
    assert_eq!(query_param(&u, "redirect_uri"), want);
    assert_ne!(query_param(&u, "state"), "theirs");
}

#[test]
fn redirect_targets() {
    assert_eq!(
        redirect_target("http://localhost:9876/cb"),
        Ok((9876, "/cb".to_string()))
    );
    assert_eq!(
        redirect_target("http://[::1]:8080"),
        Ok((8080, "/".to_string()))
    );
    assert_eq!(
        redirect_target("https://localhost:9876/cb").unwrap_err(),
        "redirectUrl \"https://localhost:9876/cb\" must be an http:// URL on this machine"
    );
    assert_eq!(
        redirect_target("http://example.com:9876/cb").unwrap_err(),
        "redirectUrl \"http://example.com:9876/cb\" must point at localhost, where apic listens for the sign-in"
    );
    assert_eq!(
        redirect_target("http://localhost/cb").unwrap_err(),
        "redirectUrl \"http://localhost/cb\" needs a port"
    );
    assert_eq!(
        redirect_target("http://localhost:0/cb").unwrap_err(),
        "redirectUrl \"http://localhost:0/cb\" needs a port"
    );
}

#[test]
fn authorization_code_parse() {
    for bad in [
        "oauth2 grant=authorization_code tokenUrl=t clientId=c",
        "oauth2 grant=authorization_code tokenUrl=t clientId=c authUrl=a redirectPort=abc",
        "oauth2 grant=authorization_code tokenUrl=t clientId=c authUrl=a redirectPort=70000",
        "oauth2 grant=authorization_code tokenUrl=t clientId=c authUrl=a redirectPort=1 redirectUrl=http://localhost:1/",
    ] {
        assert!(parse(bad).is_err(), "{bad:?} should not parse");
    }
    let s = parse("oauth2 grant=authorization_code tokenUrl=t clientId=c authUrl=a redirectPort=8976 clientSecret=s").unwrap();
    assert_eq!(s.option("redirectPort"), "8976");
    // authUrl is part of the cache key, so two providers never share a token.
    let other = parse("oauth2 grant=authorization_code tokenUrl=t clientId=c authUrl=b redirectPort=8976 clientSecret=s").unwrap();
    assert_ne!(
        cache_key(&s),
        cache_key(&other),
        "cache key ignores authUrl"
    );
    // The other grants keep the key they had before authUrl existed, so a
    // token cached by an earlier apic is still found after upgrading.
    let cc = parse("oauth2 tokenUrl=t clientId=c clientSecret=s scope=read").unwrap();
    let old = format!(
        "$oauth2:{}",
        hash_key(
            &[
                "t",
                "",
                "c",
                "s",
                "",
                "client_credentials",
                "read",
                "",
                "",
                ""
            ]
            .join("\x00")
        )
    );
    assert_eq!(cache_key(&cc), old, "client_credentials cache key changed");
    let (verifier, challenge) = pkce().unwrap();
    assert_eq!(verifier.len(), 43);
    assert_eq!(
        challenge,
        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
    );
    // A cache written elsewhere still reads.
    let cache = MemCache::default();
    cache.set("k", "v").unwrap();
    assert_eq!(cache.get("k").as_deref(), Some("v"));
}
