use apic_gocompat::Time;

use super::refs::ref_project;
use super::*;
use crate::{parse_result, MASKED};

#[test]
fn history_records_each_request_a_ref_ran() {
    let (dir, _logins, _srv) = ref_project();
    std::fs::write(dir.path().join("apic.yaml"), "history: 3\n").unwrap();
    let mut r = new_runner(
        dir.path(),
        Options {
            env: "dev".into(),
            session: Some(memory_session()),
            ..Options::default()
        },
    );
    let at = Time::from_civil(2026, 10, 6, 12, 0, 0, 0, 0);
    r.now = Some(Box::new(move || at));
    let history = r
        .history
        .clone()
        .expect("history: 3 should switch history on");
    let res = run_ok(&r, "whoami");
    assert!(res.ok, "whoami: {res:?}");
    assert!(res.recorded && res.record_key == "whoami" && res.deps[0].recorded);
    for name in ["login", "whoami"] {
        let entries = history.list("dev", name).unwrap();
        assert_eq!(entries.len(), 1, "{name}");
        assert!(
            entries[0].time == at && entries[0].status == 200,
            "{name} entry = {:?}",
            entries[0]
        );
    }
    let e = history.get("dev", "whoami", 1).unwrap();
    let stored = parse_result(e.result.as_deref().unwrap()).unwrap();
    let raw = stored.raw().expect("a stored response");
    assert!(
        stored.deps.is_empty() && stored.request.name == "whoami" && raw.status == 200,
        "stored whoami = {stored:?}"
    );
    assert_eq!(
        String::from_utf8_lossy(&raw.body),
        r#"{"email":"alice@example.com","token":"tok-1"}"#
    );
    let authz = stored
        .request
        .headers
        .iter()
        .find(|h| h.name == "Authorization")
        .expect("the header is stored");
    assert_eq!(authz.value, MASKED);
    // What was stored reads back as it was written.
    assert_eq!(stored.to_json(), {
        let mut again = res.clone();
        again.deps.clear();
        // The stored body is the document re-written with sorted keys.
        again.to_json()
    });
}

#[test]
fn no_history_and_no_session_record_nothing() {
    let (dir, _logins, _srv) = ref_project();
    std::fs::write(dir.path().join("apic.yaml"), "history: 3\n").unwrap();
    for (no_session, no_history) in [(true, false), (false, true)] {
        let r = new_runner(
            dir.path(),
            Options {
                env: "dev".into(),
                no_session,
                no_history,
                ..Options::default()
            },
        );
        assert!(r.history.is_none(), "history is on");
    }
}
