use std::sync::Arc;

use cvdb::fake::FakeServer;
use cvws::{Backend, Store, StoreError, Update};
use serde_json::json;

const WS: &str = "ws1";

fn open(server: &FakeServer, dir: &std::path::Path) -> Backend {
    Backend::open_remote(Arc::new(server.clone()), "https://cv.invalid/api", WS, dir).unwrap()
}

fn update<'a>(key: &'a str, expected_clock: u64, bytes: &'a [u8], op_id: &str) -> Update<'a> {
    let mut metadata = serde_json::Map::new();
    metadata.insert("op".into(), json!({ "kind": "setField" }));
    metadata.insert("opId".into(), json!(op_id));
    Update {
        schema_id: "careervector.yjs-subdoc/v1",
        key,
        expected_clock,
        snapshot: b"",
        state_vector: b"",
        bytes,
        actor_class: "human",
        actor_id: "u1",
        device: "d",
        at_ms: 42,
        metadata,
    }
}

#[test]
fn receipts_cas_and_tail_catch_up_through_the_store_trait() {
    let server = FakeServer::new();
    let dir = tempfile::tempdir().unwrap();
    let s = open(&server, dir.path());
    assert_eq!(s.get(WS, "jobs").unwrap(), None);
    let r1 = s.append_update(WS, &update("jobs", 0, b"u1", "a")).unwrap();
    let r2 = s.append_update(WS, &update("jobs", 1, b"u2", "b")).unwrap();
    assert_eq!((r1.clock, r2.clock, r2.seq), (Some(1), Some(2), 2));
    assert_eq!(s.receipts(WS, 1).unwrap(), vec![r2]);
    // A stale clock is refused, and the server's tail is untouched.
    let stale = s.append_update(WS, &update("jobs", 1, b"x", "c"));
    assert!(matches!(stale, Err(StoreError::Conflict { .. })));
    // Another writer appends; catch-up reads exactly the updates after our clock.
    server.external_append("jobs", b"theirs");
    let ops = s.ops_since(WS, "jobs", 2).unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!((ops[0].clock, ops[0].bytes.as_slice()), (3, &b"theirs"[..]));
    assert_eq!(s.ops_since(WS, "jobs", 0).unwrap().len(), 3);
    // After a fold the early updates are gone from the tail: the caller must reload the head.
    server.fold("jobs", b"HEAD");
    assert!(matches!(
        s.ops_since(WS, "jobs", 1),
        Err(StoreError::Invalid(_))
    ));
    let head = s.get_record(WS, "jobs").unwrap().unwrap();
    assert_eq!(
        (head.bytes.as_slice(), head.yjs.unwrap().clock),
        (&b"HEAD"[..], 3)
    );
    // Rebased on the server's clock, the next commit goes through.
    let r = s.append_update(WS, &update("jobs", 3, b"u4", "d")).unwrap();
    assert_eq!(r.clock, Some(4));
}

#[test]
fn offline_commits_wait_in_the_outbox_and_replay() {
    let server = FakeServer::new();
    let dir = tempfile::tempdir().unwrap();
    let Backend::Remote(s) = open(&server, dir.path()) else {
        unreachable!()
    };
    server.set_offline(true);
    let c = cvws::Commit {
        record: cvws::Draft::new(
            "careervector.yjs-subdoc/v1",
            "settings",
            cvws::Kind::CrdtSnapshot,
            vec![],
        )
        .at(
            1,
            Some(cvws::Yjs {
                state_vector: vec![],
                clock: 1,
            }),
        )
        .to_record(),
        op: Some(cvws::Op {
            clock: 1,
            actor_class: "human".into(),
            actor_id: "u".into(),
            at_ms: 1,
            bytes: b"s1".to_vec(),
        }),
        at_ms: 1,
    };
    let mut c = c;
    c.record.metadata = r#"{"op":{"kind":"setField"},"opId":"x1"}"#.into();
    let queued = s.commit(WS, &c);
    assert!(
        matches!(queued, Err(StoreError::Io(ref e)) if e.kind() == std::io::ErrorKind::WouldBlock)
    );
    assert_eq!(s.queued(), 1);
    server.set_offline(false);
    let f = s.flush().unwrap();
    assert_eq!((f.acked, f.left), (1, 0));
    assert_eq!(server.tail("settings"), vec![b"s1".to_vec()]);
}

#[test]
fn what_the_server_cannot_do_is_refused() {
    let server = FakeServer::new();
    let dir = tempfile::tempdir().unwrap();
    let s = open(&server, dir.path());
    assert!(s.keys(WS).is_err());
    assert!(s.dump(WS).is_err());
    assert!(s.put_asset(WS, "a", b"x").is_err());
    assert!(s.receipts("other", 0).is_err());
}
