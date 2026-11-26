use std::fs;

use crate::{Store, DEFAULT_ENV, DIR, FILE};

#[cfg(unix)]
fn perm(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// The properties that keep a captured OAuth2 access or refresh token out
/// of a commit and out of other users' reach: 0700 on .apic/, 0600 on
/// session.json, and a .gitignore beside it.
#[test]
fn save_protects_tokens_on_disk() {
    let root = tempfile::tempdir().unwrap();
    let mut s = Store::open(root.path()).unwrap();
    s.set("dev", [("token", "secret-token")]);
    s.save().unwrap();

    let gi = root.path().join(DIR).join(".gitignore");
    let data = fs::read_to_string(&gi).expect("session directory needs a .gitignore");
    assert_eq!(
        data,
        "# created by apic; session state must not be committed\n*\n"
    );

    // File modes are not meaningful on Windows.
    #[cfg(unix)]
    {
        assert_eq!(perm(&root.path().join(DIR)), 0o700, "{DIR} mode");
        assert_eq!(
            perm(&root.path().join(DIR).join(FILE)),
            0o600,
            "{FILE} mode"
        );
        assert_eq!(perm(&gi), 0o600, ".gitignore mode");
    }
}

#[test]
fn save_open_round_trip() {
    let root = tempfile::tempdir().unwrap();
    let mut s = Store::open(root.path()).unwrap();
    s.set("dev", [("token", "t1"), ("id", "7")]);
    s.set("prod", [("token", "t2")]);
    s.save().unwrap();

    let again = Store::open(root.path()).unwrap();
    assert_eq!(again.get("dev", "token"), Some("t1"));
    assert_eq!(again.get("prod", "token"), Some("t2"));
    assert_eq!(
        again.get("dev", "nope"),
        None,
        "unknown name should not resolve"
    );
    assert_eq!(again.env_names(), ["dev", "prod"]);
}

/// The two states a fresh or damaged project can be in: no session file
/// at all is fine, unreadable JSON is not.
#[test]
fn open_missing_and_corrupt() {
    let root = tempfile::tempdir().unwrap();
    let s = Store::open(root.path()).expect("a missing session file should be fine");
    assert!(
        s.env_names().is_empty(),
        "a fresh session should be empty, got {:?}",
        s.env_names()
    );

    fs::create_dir_all(root.path().join(DIR)).unwrap();
    let file = root.path().join(DIR).join(FILE);
    fs::write(&file, "{not json").unwrap();
    assert_eq!(
        Store::open(root.path()).unwrap_err().to_string(),
        "invalid character 'n' looking for beginning of object key string",
        "a corrupt session file should be an error, not silent data loss"
    );
    for (text, want) in [
        (r#"{"envs":{"dev":{"a":1}}}"#, "json: cannot unmarshal number into Go struct field Store.envs of type string"),
        (r#"{"envs":[1]}"#, "json: cannot unmarshal array into Go struct field Store.envs of type map[string]map[string]string"),
        ("[1]", "json: cannot unmarshal array into Go value of type session.Store"),
    ] {
        fs::write(&file, text).unwrap();
        assert_eq!(Store::open(root.path()).unwrap_err().to_string(), want);
    }
    fs::write(&file, r#"{"envs":{"dev":{"a":null},"x":null},"other":1}"#).unwrap();
    let s = Store::open(root.path()).unwrap();
    assert_eq!(s.get("dev", "a"), Some(""));
    assert_eq!(s.env_names(), ["dev"]);
}

/// "" and the default name address the same bucket, so a run without
/// --env finds what an earlier one captured.
#[test]
fn default_env_key() {
    let mut s = Store::new_memory();
    s.set("", [("token", "t")]);
    assert_eq!(s.get(DEFAULT_ENV, "token"), Some("t"));
    assert_eq!(s.get("", "token"), Some("t"));
}

#[test]
fn vars_is_a_copy() {
    let mut s = Store::new_memory();
    s.set("dev", [("token", "t")]);
    let mut vars = s.vars("dev");
    vars.insert("token".to_string(), "mutated".to_string());
    vars.insert("extra".to_string(), "added".to_string());
    assert_eq!(
        s.get("dev", "token"),
        Some("t"),
        "vars should hand out a copy"
    );
    assert_eq!(s.get("dev", "extra"), None, "vars should hand out a copy");
}

#[test]
fn clear() {
    let mut s = Store::new_memory();
    s.set("dev", [("token", "t")]);
    s.set("prod", [("token", "t")]);

    s.clear("dev");
    assert_eq!(s.get("dev", "token"), None, "dev should be cleared");
    assert!(
        s.get("prod", "token").is_some(),
        "prod should survive clearing dev"
    );

    s.clear("*");
    assert!(
        s.env_names().is_empty(),
        "clear(\"*\") should drop everything"
    );
}

#[test]
fn snapshot_is_separate_and_memory_only() {
    let root = tempfile::tempdir().unwrap();
    let mut s = Store::open(root.path()).unwrap();
    s.set("dev", [("token", "t")]);
    let mut copy = s.snapshot();
    copy.set("dev", [("token", "changed"), ("more", "1")]);
    copy.save().unwrap();
    assert_eq!(s.get("dev", "token"), Some("t"));
    assert_eq!(copy.get("dev", "token"), Some("changed"));
    assert!(!root.path().join(DIR).exists(), "a snapshot writes nothing");
}

/// --no-session: nothing reaches the disk.
#[test]
fn memory_store_writes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let mut s = Store::new_memory();
    s.set("dev", [("token", "secret-token")]);
    s.save().unwrap();
    assert_eq!(
        fs::read_dir(root.path()).unwrap().count(),
        0,
        "a memory store should write nothing"
    );
}

/// The on-disk format, which a user may inspect, which `open` must keep
/// reading, and which is shared byte for byte with earlier versions.
#[test]
fn saved_file_shape() {
    let root = tempfile::tempdir().unwrap();
    let mut s = Store::open(root.path()).unwrap();
    s.set(
        "dev",
        [("token", "a<b>&c\u{2028}\u{7f}\u{1}é"), ("id", "7")],
    );
    s.set("", Vec::<(String, String)>::new());
    s.save().unwrap();
    let data = fs::read_to_string(root.path().join(DIR).join(FILE)).unwrap();
    assert_eq!(
        data,
        "{\n  \"envs\": {\n    \"default\": {},\n    \"dev\": {\n      \"id\": \"7\",\n      \"token\": \"a\\u003cb\\u003e\\u0026c\\u2028\u{7f}\\u0001é\"\n    }\n  }\n}\n"
    );
    let mut empty = Store::open(tempfile::tempdir().unwrap().path()).unwrap();
    empty.clear("*");
    let root = tempfile::tempdir().unwrap();
    let empty = Store::open(root.path()).unwrap();
    empty.save().unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join(DIR).join(FILE)).unwrap(),
        "{\n  \"envs\": {}\n}\n"
    );
}

/// apic refuses to drop OAuth2 tokens into a directory it could not mark
/// as ignored, rather than swallowing the error and writing them anyway.
///
/// The .gitignore is a dangling symlink into a directory that does not
/// exist: looking it up follows the link and finds nothing (so save tries
/// to create it), and the write then fails. That works whatever the uid,
/// unlike chmod, which root ignores.
#[cfg(unix)]
#[test]
fn save_fails_if_gitignore_cannot_be_written() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join(DIR);
    fs::create_dir_all(&dir).unwrap();
    let gi = dir.join(".gitignore");
    std::os::unix::fs::symlink(root.path().join("no-such-dir").join("target"), &gi).unwrap();

    let mut s = Store::open(root.path()).unwrap();
    s.set("dev", [("token", "secret-token")]);
    let err = s
        .save()
        .expect_err("save should fail when the .gitignore cannot be created");
    assert_eq!(
        err.to_string(),
        format!("{0}: open {0}: no such file or directory", gi.display()),
        "the error should name the file"
    );
    assert!(
        !dir.join(FILE).exists(),
        "tokens should not be written when the directory could not be marked ignored"
    );
}
