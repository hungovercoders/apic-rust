use std::sync::Arc;

use apic_runner::{shared_session, Options, Runner};

use super::ascii;
use crate::{Config, Model, Msg};

// A directory with no request files opens, and the list says what would
// give it some rather than only that nothing matches.
#[test]
fn empty_project_points_at_init_and_demo() {
    ascii(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = apic_project::load(dir.path()).expect("load");
        let opts = Options {
            session: Some(shared_session(apic_session::Store::new_memory())),
            ..Options::default()
        };
        let r = Arc::new(Runner::new(p, opts).expect("runner"));
        let mut cfg = Config::new(r);
        cfg.root = dir.path().to_str().expect("utf-8 path").to_string();
        let mut m = Model::new(cfg);
        m.update(Msg::Size {
            width: 120,
            height: 40,
        });
        let v = m.view();
        for want in [
            "no .http files here",
            "apic init scaffolds a project",
            "apic ui --demo shows a working one",
        ] {
            assert!(v.contains(want), "missing {want:?} in:\n{v}");
        }
        assert!(
            !v.contains("no requests match"),
            "an empty project is not a filter with no matches:\n{v}"
        );
    });
}
