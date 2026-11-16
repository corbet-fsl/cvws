use cvws::{Backend, Draft, Envelope, Kind, Origin, Store, StoreError, Update, convert};
use serde_json::json;

const WS: &str = "ws1";

fn backends() -> (tempfile::TempDir, Vec<(&'static str, Backend)>) {
    let dir = tempfile::tempdir().unwrap();
    let fs = Backend::open_fs(dir.path().join("fs")).unwrap();
    let db = Backend::open_sqlite(dir.path().join("cv.db")).unwrap();
    (dir, vec![("fs", fs), ("sqlite", db)])
}

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

fn update<'a>(
    key: &'a str,
    expected_clock: u64,
    snapshot: &'a [u8],
    bytes: &'a [u8],
) -> Update<'a> {
    Update {
        schema_id: "careervector.yjs-subdoc/v1",
        key,
        expected_clock,
        snapshot,
        state_vector: &[9, 8, 7],
        bytes,
        actor_class: "human",
        actor_id: "u1",
        device: "laptop",
        at_ms: 1_700_000_000_000 + expected_clock,
        metadata: serde_json::Map::new(),
    }
}

/// M1 on every backend: authored bytes, revisions, hashes and receipts survive a round trip.
#[test]
fn m1_round_trip_preserves_bytes_revisions_and_receipts() {
    let (_d, all) = backends();
    for (name, s) in &all {
        let raw = b"[cv]\r\ntitle = \"\xff\xfe\"\0 no final newline";
        let r1 = s.put(WS, draft("cv.toml", raw), 0, 10).unwrap();
        let r2 = s.put(WS, draft("cv.toml", b"v2"), 1, 20).unwrap();
        assert_eq!(
            (r1.seq, r1.revision, r2.seq, r2.revision),
            (1, 1, 2, 2),
            "{name}"
        );
        let e = s.get(WS, "cv.toml").unwrap().unwrap();
        assert_eq!(
            (e.revision, e.bytes.as_slice(), e.kind),
            (2, &b"v2"[..], Kind::Projected),
            "{name}"
        );
        assert!(e.verify(), "{name}");
        assert_eq!(
            e.metadata["unknown"],
            json!({"keep": [1, "two", null]}),
            "{name}"
        );
        assert_eq!(e.origin.as_ref().unwrap().parent_revision, 1, "{name}");
        let first = &s.retained(WS, "cv.toml").unwrap()[0];
        assert_eq!(
            (first.revision, first.bytes.as_slice()),
            (1, &raw[..]),
            "{name}"
        );
        assert_eq!(first.content_hash, r1.content_hash, "{name}");
        assert_eq!(s.receipts(WS, 0).unwrap(), vec![r1, r2], "{name}");
    }
}

#[test]
fn cas_conflicts_are_refused() {
    let (_d, all) = backends();
    for (name, s) in &all {
        s.put(WS, draft("k", b"one"), 0, 1).unwrap();
        // A second writer that read nothing, and one that read an old revision.
        let again = s.put(WS, draft("k", b"x"), 0, 2);
        assert!(
            matches!(
                again,
                Err(StoreError::Conflict {
                    expected: 0,
                    actual: 1,
                    ..
                })
            ),
            "{name}"
        );
        s.put(WS, draft("k", b"two"), 1, 3).unwrap();
        let stale = s.put(WS, draft("k", b"y"), 1, 4);
        assert!(
            matches!(
                stale,
                Err(StoreError::Conflict {
                    expected: 1,
                    actual: 2,
                    ..
                })
            ),
            "{name}"
        );
        assert_eq!(s.get(WS, "k").unwrap().unwrap().bytes, b"two", "{name}");
        // Yjs: the clock is the condition.
        s.append_update(WS, &update("jobs", 0, b"s1", b"u1"))
            .unwrap();
        s.append_update(WS, &update("jobs", 1, b"s2", b"u2"))
            .unwrap();
        let stale = s.append_update(WS, &update("jobs", 1, b"s3", b"u3"));
        assert!(
            matches!(
                stale,
                Err(StoreError::Conflict {
                    expected: 1,
                    actual: 2,
                    ..
                })
            ),
            "{name}"
        );
        let jobs = s.get(WS, "jobs").unwrap().unwrap();
        assert_eq!(jobs.yjs.as_ref().unwrap().clock, 2, "{name}");
        assert_eq!(jobs.kind, Kind::CrdtSnapshot, "{name}");
        let ops = s.ops_since(WS, "jobs", 0).unwrap();
        assert_eq!(
            ops.iter().map(|o| o.bytes.clone()).collect::<Vec<_>>(),
            vec![b"u1".to_vec(), b"u2".to_vec()],
            "{name}"
        );
        // A tampered hash is refused.
        let mut e = draft("k", b"z").at(3, None);
        e.content_hash = cvws::content_hash(b"other");
        let bad = s.commit(
            WS,
            &cvws::Commit {
                record: e.to_record(),
                op: None,
                at_ms: 5,
            },
        );
        assert!(matches!(bad, Err(StoreError::Integrity(_))), "{name}");
    }
}

fn fill(s: &Backend) {
    s.put(WS, draft("cv.toml", b"\r\nraw\xff"), 0, 10).unwrap();
    s.put(WS, draft("cv.toml", b"v2"), 1, 20).unwrap();
    s.put(WS, draft("letter", b"l1"), 0, 30).unwrap();
    s.append_update(WS, &update("jobs", 0, b"snap1", b"up1"))
        .unwrap();
    s.append_update(WS, &update("jobs", 1, b"snap2", b"up2"))
        .unwrap();
    s.put_asset(WS, "img/photo.png", b"\x89PNG\r\n").unwrap();
}

fn envelopes(s: &Backend) -> Vec<Envelope> {
    s.keys(WS)
        .unwrap()
        .iter()
        .map(|k| s.get(WS, k).unwrap().unwrap())
        .collect()
}

/// M2: fs to db and db to fs conversions are lossless for the same envelope.
#[test]
fn m2_fs_db_conversion_is_lossless_both_ways() {
    let dir = tempfile::tempdir().unwrap();
    let fs1 = Backend::open_fs(dir.path().join("fs1")).unwrap();
    fill(&fs1);
    let db = Backend::open_sqlite(dir.path().join("a.db")).unwrap();
    let dump = convert(&fs1, &db, WS).unwrap();
    assert_eq!(dump.records.len(), 3);
    assert_eq!(dump.ops.len(), 2);
    assert_eq!(dump.receipts.len(), 5);
    assert_eq!(dump.retained.len(), 2);
    assert_eq!(envelopes(&fs1), envelopes(&db));
    assert_eq!(fs1.receipts(WS, 0).unwrap(), db.receipts(WS, 0).unwrap());
    assert_eq!(
        fs1.ops_since(WS, "jobs", 0).unwrap(),
        db.ops_since(WS, "jobs", 0).unwrap()
    );
    assert_eq!(
        db.get_asset(WS, "img/photo.png").unwrap().unwrap(),
        b"\x89PNG\r\n"
    );

    // And back, into a fresh directory.
    let fs2 = Backend::open_fs(dir.path().join("fs2")).unwrap();
    assert_eq!(convert(&db, &fs2, WS).unwrap(), dump);
    assert_eq!(envelopes(&fs2), envelopes(&fs1));

    // The converted workspace carries on where the original stopped, on either backend.
    let next_db = db.put(WS, draft("letter", b"l2"), 1, 40).unwrap();
    let next_fs = fs2.put(WS, draft("letter", b"l2"), 1, 40).unwrap();
    assert_eq!(next_db, next_fs);
    assert_eq!(next_db.seq, 6);
    let again = db
        .append_update(WS, &update("jobs", 2, b"snap3", b"up3"))
        .unwrap();
    assert_eq!(again.clock, Some(3));
    // A non-empty destination is refused.
    assert!(convert(&fs1, &fs2, WS).is_err());
}
