//! The five calls and the two for server-applied ops, on small synthetic documents.
#![allow(clippy::needless_pass_by_value)]
use cvws::{ActorClass, SUB_DOCS, SubDoc, Workspace};
use serde_json::{Value, json};
use yrs::{Doc as YDoc, Map, ReadTxn, Transact};

const CV: &str = "cv_profile.en";
const CL: &str = "cl_profile.en";

fn node(id: &str, kind: &str, data: Value, children: Vec<Value>) -> Value {
    let active: Vec<Value> = children.iter().map(|c| c["id"].clone()).collect();
    json!({ "id": id, "kind": kind, "tier": "HYBRID", "data": data,
            "selection": { "active": active }, "children": children })
}

fn cv_tree() -> Value {
    node(
        "root",
        "cv-root",
        json!({}),
        vec![node(
            "item",
            "list-item",
            json!({ "heading": "Engineer" }),
            vec![node(
                "b1",
                "bullet",
                json!({ "text": "Built things" }),
                vec![],
            )],
        )],
    )
}

fn cl_tree() -> Value {
    node(
        "cl-root",
        "cl-root",
        json!({}),
        vec![node(
            "body",
            "body",
            json!({ "archetype": "text", "text": "Dear team" }),
            vec![],
        )],
    )
}

fn replace(kind: &str, tree: &Value) -> Value {
    json!({ "kind": kind, "lang": "en", "tree": tree })
}

fn seeded() -> Workspace {
    let mut ws = Workspace::open(&[]).expect("open");
    ws.apply(
        &replace("cv_profile.replace", &cv_tree()),
        ActorClass::Human,
    )
    .expect("cv");
    ws.apply(
        &replace("cl_profile.replace", &cl_tree()),
        ActorClass::Human,
    )
    .expect("cl");
    ws
}

fn text_of(ws: &Workspace, sub: SubDoc, branch: &str, path: &str) -> String {
    ws.view(sub, branch, path).expect("view")["data"]["text"]
        .as_str()
        .expect("text")
        .to_string()
}

#[test]
fn apply_routes_by_branch_and_reports_changed_ids_per_sub_doc() {
    let mut ws = seeded();
    let applied = ws
        .apply(
            &json!({ "kind": "node.update", "branch_path": CL, "node_path": "cl-root.body",
                     "patch": { "data": { "text": "Dear all" } } }),
            ActorClass::Human,
        )
        .expect("apply");
    assert_eq!(applied.changed.len(), 1);
    assert_eq!(
        applied.of(SubDoc::ClProfile).expect("cl").nodes,
        vec!["body"]
    );
    assert!(applied.of(SubDoc::CvProfile).is_none());
    assert_eq!(
        text_of(&ws, SubDoc::ClProfile, CL, "cl-root.body"),
        "Dear all"
    );
}

#[test]
fn outgoing_bytes_are_tagged_per_sub_doc_and_taken_once() {
    let mut ws = seeded();
    let out = ws.take_outgoing();
    assert_eq!(out.len(), 2);
    assert_eq!(
        out.iter().map(|o| o.sub_doc).collect::<Vec<_>>(),
        SUB_DOCS.to_vec()
    );
    assert_eq!(ws.take_outgoing().len(), 0);

    // The bytes rebuild the workspace elsewhere.
    let cv = out
        .iter()
        .find(|o| o.sub_doc == SubDoc::CvProfile)
        .expect("cv bytes");
    let mut peer = Workspace::open(&[]).expect("peer");
    let applied = peer
        .receive(&cv.update, Some(SubDoc::CvProfile))
        .expect("receive");
    assert!(
        applied
            .of(SubDoc::CvProfile)
            .expect("changed")
            .nodes
            .contains(&"b1".to_string())
    );
    assert_eq!(
        text_of(&peer, SubDoc::CvProfile, CV, "root.item.b1"),
        "Built things"
    );
}

#[test]
fn a_mixed_batch_applies_to_both_sub_docs_or_to_neither() {
    let mut ws = seeded();
    ws.take_outgoing();
    let bad = json!({ "kind": "batch", "ops": [
        { "subDoc": "cv-profile", "op": { "kind": "node.update", "branch_path": CV, "node_path": "root.item.b1", "patch": { "tier": "OPEN" } } },
        { "subDoc": "cl-profile", "op": { "kind": "node.update", "branch_path": CL, "node_path": "cl-root.ghost", "patch": { "tier": "OPEN" } } },
    ] });
    assert!(ws.apply(&bad, ActorClass::Human).is_err());
    assert_eq!(
        ws.view(SubDoc::CvProfile, CV, "root.item.b1")
            .expect("view")["tier"],
        "HYBRID"
    );
    assert!(
        ws.take_outgoing().is_empty(),
        "a refused batch leaves nothing to send"
    );

    let good = json!({ "kind": "batch", "ops": [
        { "subDoc": "cv-profile", "op": { "kind": "node.update", "branch_path": CV, "node_path": "root.item.b1", "patch": { "tier": "OPEN" } } },
        { "op": { "kind": "node.update", "branch_path": CL, "node_path": "cl-root.body", "patch": { "tier": "OPEN" } } },
    ] });
    let applied = ws.apply(&good, ActorClass::Human).expect("both");
    assert_eq!(applied.changed.len(), 2);
    assert_eq!(
        ws.view(SubDoc::CvProfile, CV, "root.item.b1")
            .expect("view")["tier"],
        "OPEN"
    );
    assert_eq!(
        ws.view(SubDoc::ClProfile, CL, "cl-root.body")
            .expect("view")["tier"],
        "OPEN"
    );
}

#[test]
fn a_batch_sub_op_declared_for_the_wrong_sub_doc_is_refused() {
    let mut ws = seeded();
    let wrong = json!({ "kind": "batch", "ops": [
        { "subDoc": "cl-profile", "op": { "kind": "node.update", "branch_path": CV, "node_path": "root.item.b1", "patch": { "tier": "OPEN" } } },
    ] });
    let err = ws.apply(&wrong, ActorClass::Human).expect_err("refused");
    assert!(
        err.message().contains("declared for sub-doc 'cl-profile'"),
        "{err}"
    );
}

#[test]
fn ops_for_other_sub_docs_are_not_ours() {
    let mut ws = seeded();
    for op in [
        json!({ "kind": "job.create", "url": "https://example.org/job" }),
        json!({ "kind": "settings.update", "patch": {} }),
        json!({ "kind": "tailored.snapshot", "job_id": "j", "branch_path": CV }),
        json!({ "kind": "node.frobnicate" }),
    ] {
        let err = ws
            .apply(&op, ActorClass::Human)
            .expect_err("not a profile op");
        assert!(err.message().starts_with("cvws:"), "{err}");
    }
    assert!(
        ws.apply(&json!({ "kind": "batch", "ops": [] }), ActorClass::Human)
            .expect("empty")
            .is_empty()
    );
}

#[test]
fn prepare_validates_without_applying_and_the_server_bytes_come_back_through_receive() {
    let mut ws = seeded();
    ws.take_outgoing();
    let op = json!({ "kind": "node.update", "branch_path": CV, "node_path": "root.item.b1",
                     "patch": { "data": { "text": "Built great things" } } });

    // Build and check the envelope; nothing is applied, nothing is outgoing.
    let prepared = ws.prepare(&op, ActorClass::Human).expect("prepared");
    assert_eq!(prepared.routes, vec![SubDoc::CvProfile]);
    assert_eq!(
        prepared
            .would_change
            .of(SubDoc::CvProfile)
            .expect("would")
            .nodes,
        vec!["b1"]
    );
    assert!(
        prepared.envelope["opId"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    assert_eq!(
        text_of(&ws, SubDoc::CvProfile, CV, "root.item.b1"),
        "Built things"
    );
    assert_eq!(ws.take_outgoing().len(), 0);

    // A caller-supplied opId is kept; a refused op is reported before sending.
    let with_id = json!({ "kind": op["kind"], "branch_path": CV, "node_path": "root.item.b1", "patch": {}, "opId": "mine" });
    assert_eq!(
        ws.prepare(&with_id, ActorClass::Human)
            .expect("kept")
            .envelope["opId"],
        "mine"
    );
    let bad = json!({ "kind": "node.delete", "branch_path": CV, "node_path": "root.ghost.x" });
    assert!(ws.prepare(&bad, ActorClass::Human).is_err());

    // The server applies the op under its own client id and fans the bytes out.
    let mut server = Workspace::open(&[(SubDoc::CvProfile, &ws.encode_state(SubDoc::CvProfile))])
        .expect("server");
    server
        .apply(&prepared.envelope, ActorClass::Human)
        .expect("server applies");
    let fanout = server.take_outgoing().remove(0);

    // The client receives them: the edit appears exactly once ("great", not doubled).
    let applied = ws.receive(&fanout.update, None).expect("receive");
    assert_eq!(
        applied.of(SubDoc::CvProfile).expect("changed").nodes,
        vec!["b1"]
    );
    assert_eq!(
        text_of(&ws, SubDoc::CvProfile, CV, "root.item.b1"),
        "Built great things"
    );
    assert!(
        ws.take_outgoing().is_empty(),
        "received updates are not echoed"
    );
}

#[test]
fn a_frame_without_a_sub_doc_tag_is_offered_to_every_sub_doc() {
    let mut ws = seeded();
    ws.take_outgoing();
    let before: Vec<Vec<u8>> = SUB_DOCS.iter().map(|s| ws.encode_state(*s)).collect();

    // An update for a different sub-document (a `settings` root) changes neither.
    let foreign = YDoc::new();
    let settings = foreign.get_or_insert_map("settings");
    {
        let mut txn = foreign.transact_mut();
        settings.insert(&mut txn, "theme", "dark");
    }
    let bytes = foreign
        .transact()
        .encode_state_as_update_v1(&yrs::StateVector::default());
    let applied = ws.receive(&bytes, None).expect("offered");
    assert!(applied.is_empty());
    for (i, sub) in SUB_DOCS.iter().enumerate() {
        assert_eq!(
            ws.encode_state(*sub),
            before[i],
            "{sub:?} kept no foreign root"
        );
        assert!(!ws.has_pending(*sub), "{sub:?} kept no pending state");
    }

    // An update for the cover letter reaches the cover letter only.
    let mut other = Workspace::open(&[(SubDoc::ClProfile, &ws.encode_state(SubDoc::ClProfile))])
        .expect("other");
    other
        .type_text(SubDoc::ClProfile, CL, "cl-root.body", "text", 0..0, "Hi. ")
        .expect("typed");
    let update = other.take_outgoing().remove(0).update;
    let applied = ws.receive(&update, None).expect("offered");
    assert_eq!(applied.changed.len(), 1);
    assert_eq!(
        applied.of(SubDoc::ClProfile).expect("cl").nodes,
        vec!["body"]
    );
    assert_eq!(
        ws.encode_state(SubDoc::CvProfile),
        before[0],
        "the CV sub-doc is untouched"
    );
    assert!(!ws.has_pending(SubDoc::CvProfile));
    assert_eq!(
        text_of(&ws, SubDoc::ClProfile, CL, "cl-root.body"),
        "Hi. Dear team"
    );

    // The same bytes again change nothing.
    assert!(ws.receive(&update, None).expect("again").is_empty());
}

#[test]
fn a_hint_sends_the_update_to_one_sub_doc() {
    let mut ws = seeded();
    ws.take_outgoing();
    let mut other = Workspace::open(&[(SubDoc::CvProfile, &ws.encode_state(SubDoc::CvProfile))])
        .expect("other");
    other
        .type_text(SubDoc::CvProfile, CV, "root.item.b1", "text", 0..0, ">")
        .expect("typed");
    let update = other.take_outgoing().remove(0).update;
    let applied = ws
        .receive(&update, Some(SubDoc::CvProfile))
        .expect("hinted");
    assert_eq!(applied.changed.len(), 1);
    assert_eq!(
        text_of(&ws, SubDoc::CvProfile, CV, "root.item.b1"),
        ">Built things"
    );
}

#[test]
fn open_restores_stored_state_and_type_text_is_the_keystroke_path() {
    let mut ws = seeded();
    ws.type_text(
        SubDoc::CvProfile,
        CV,
        "root.item.b1",
        "text",
        6..12,
        "systems",
    )
    .expect("typed");
    let stored: Vec<(SubDoc, Vec<u8>)> =
        SUB_DOCS.iter().map(|s| (*s, ws.encode_state(*s))).collect();
    let borrowed: Vec<(SubDoc, &[u8])> = stored.iter().map(|(s, b)| (*s, b.as_slice())).collect();
    let reopened = Workspace::open(&borrowed).expect("reopen");
    assert_eq!(
        text_of(&reopened, SubDoc::CvProfile, CV, "root.item.b1"),
        "Built systems"
    );
    assert_eq!(
        text_of(&reopened, SubDoc::ClProfile, CL, "cl-root.body"),
        "Dear team"
    );
}

#[test]
fn sub_doc_names_round_trip() {
    for sub in SUB_DOCS {
        assert_eq!(SubDoc::parse(sub.as_str()), Some(sub));
    }
    assert_eq!(SubDoc::parse("jobs"), None);
}
