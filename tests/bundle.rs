use std::sync::Arc;

use cvdb::fake::FakeServer;
use cvws::{
    Backend, Draft, Kind, Origin, Store, StoreError, Update, export_bundle, export_bundle_with,
    import_bundle,
};
use serde_json::{Value, json};

const WS: &str = "ws1";

fn draft(key: &str, bytes: &[u8]) -> Draft {
    let mut d = Draft::new(
        "careervector.node-tree/v1",
        key,
        Kind::Projected,
        bytes.to_vec(),
    );
    d.metadata
        .insert("unknown".into(), json!({"keep": [1, "two", null]}));
    d.origin = Some(Origin {
        actor: "human".into(),
        device: "laptop".into(),
        parent_revision: 0,
    });
    d
}

fn update<'a>(key: &'a str, clock: u64, snapshot: &'a [u8], bytes: &'a [u8]) -> Update<'a> {
    let mut metadata = serde_json::Map::new();
    metadata.insert("op".into(), json!({"kind": "setField"}));
    metadata.insert("opId".into(), json!(format!("{key}-{clock}")));
    Update {
        schema_id: "careervector.yjs-subdoc/v1",
        key,
        expected_clock: clock,
        snapshot,
        state_vector: &[9, 8, 7],
        bytes,
        actor_class: "human",
        actor_id: "u1",
        device: "laptop",
        at_ms: 1_700_000_000_000 + clock,
        metadata,
    }
}

fn fill(s: &Backend) {
    s.put(
        WS,
        draft("cv.toml", b"[cv]\r\ntitle = \"\xff\xfe\"\0 raw"),
        0,
        10,
    )
    .unwrap();
    s.put(WS, draft("cv.toml", b"v2"), 1, 20).unwrap();
    s.put(WS, draft("style.toml", b"s"), 0, 30).unwrap();
    s.append_update(WS, &update("jobs", 0, b"snap1", b"u1"))
        .unwrap();
    s.append_update(WS, &update("jobs", 1, b"snap2", b"u2"))
        .unwrap();
    s.append_update(WS, &update("notes", 0, b"n1", b"nu1"))
        .unwrap();
    s.put_asset(WS, "logos/a.png", b"\x89PNG\0\xff").unwrap();
    s.put_asset(WS, "b.bin", b"").unwrap();
}

fn local() -> (tempfile::TempDir, Backend, Backend, Backend, Backend) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let a = Backend::open_fs(p.join("fs-a")).unwrap();
    let b = Backend::open_fs(p.join("fs-b")).unwrap();
    let c = Backend::open_sqlite(p.join("a.db")).unwrap();
    let d = Backend::open_sqlite(p.join("b.db")).unwrap();
    (dir, a, b, c, d)
}

#[test]
fn export_import_export_is_byte_identical_on_fs_and_sqlite() {
    let (_d, fs_a, fs_b, db_a, db_b) = local();
    for (src, dst) in [(&fs_a, &fs_b), (&db_a, &db_b)] {
        fill(src);
        let one = export_bundle(src, WS).unwrap();
        import_bundle(dst, WS, &one).unwrap();
        assert_eq!(export_bundle(dst, WS).unwrap(), one);
    }
}

#[test]
fn fs_to_db_to_fs_through_a_bundle_is_lossless() {
    let (_d, fs_a, fs_b, db, _) = local();
    fill(&fs_a);
    let one = export_bundle(&fs_a, WS).unwrap();
    import_bundle(&db, WS, &one).unwrap();
    let two = export_bundle(&db, WS).unwrap();
    assert_eq!(two, one);
    import_bundle(&fs_b, WS, &two).unwrap();
    assert_eq!(export_bundle(&fs_b, WS).unwrap(), one);
    assert_eq!(fs_a.dump(WS).unwrap(), fs_b.dump(WS).unwrap());
}

fn remote(server: &FakeServer, dir: &std::path::Path) -> Backend {
    let b =
        Backend::open_remote(Arc::new(server.clone()), "https://cv.invalid/api", WS, dir).unwrap();
    let Backend::Remote(r) = &b else {
        unreachable!()
    };
    r.track(&["jobs", "settings"]);
    b
}

#[test]
fn export_import_export_is_byte_identical_on_the_remote_backend() {
    let (s1, s2) = (FakeServer::new(), FakeServer::new());
    let (d1, d2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let a = remote(&s1, d1.path());
    let b = remote(&s2, d2.path());
    a.append_update(WS, &update("jobs", 0, b"", b"u1")).unwrap();
    a.append_update(WS, &update("jobs", 1, b"", b"u2")).unwrap();
    a.append_update(WS, &update("settings", 0, b"", b"s1"))
        .unwrap();
    s1.external_append("jobs", b"theirs");
    let one = export_bundle(&a, WS).unwrap();
    import_bundle(&b, WS, &one).unwrap();
    assert_eq!(export_bundle(&b, WS).unwrap(), one);
    assert_eq!(
        s2.tail("jobs"),
        vec![b"u1".to_vec(), b"u2".to_vec(), b"theirs".to_vec()]
    );
    // A non-empty server workspace is refused.
    assert!(import_bundle(&b, WS, &one).is_err());
}

#[test]
fn a_remote_bundle_moves_into_a_database_and_back() {
    let s1 = FakeServer::new();
    let d1 = tempfile::tempdir().unwrap();
    let a = remote(&s1, d1.path());
    a.append_update(WS, &update("jobs", 0, b"", b"u1")).unwrap();
    let one = export_bundle(&a, WS).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let db = Backend::open_sqlite(dir.path().join("x.db")).unwrap();
    // The bundle is storage-agnostic, so a database accepts it and re-exports the same bytes.
    import_bundle(&db, WS, &one).unwrap();
    assert_eq!(export_bundle(&db, WS).unwrap(), one);
}

#[test]
fn unknown_top_level_fields_survive_and_unknown_entry_fields_are_refused() {
    let (_d, a, b, _, _) = local();
    fill(&a);
    let mut extra = serde_json::Map::new();
    extra.insert("future".into(), json!({"x": [1, 2]}));
    let one = export_bundle_with(&a, WS, &extra).unwrap();
    let got = import_bundle(&b, WS, &one).unwrap();
    assert_eq!(got.extra, extra);
    assert_eq!(export_bundle_with(&b, WS, &got.extra).unwrap(), one);

    let mut v: Value = serde_json::from_slice(&one).unwrap();
    v["receipts"][0]["new_field"] = json!(1);
    let (_d2, c, _, _, _) = local();
    let r = import_bundle(&c, WS, &serde_json::to_vec(&v).unwrap());
    assert!(matches!(r, Err(StoreError::Invalid(_))));
}

#[test]
fn a_tampered_bundle_is_refused() {
    let (_d, a, b, _, _) = local();
    fill(&a);
    let one = export_bundle(&a, WS).unwrap();
    let mut v: Value = serde_json::from_slice(&one).unwrap();
    v["assets"][0]["bytes"] = json!("00");
    let r = import_bundle(&b, WS, &serde_json::to_vec(&v).unwrap());
    assert!(matches!(r, Err(StoreError::Integrity(_))));
    assert_eq!(b.keys(WS).unwrap(), Vec::<String>::new());
}
