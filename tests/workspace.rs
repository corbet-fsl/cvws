//! The five calls and the two for server-applied ops, on small synthetic documents.
#![allow(clippy::needless_pass_by_value)]
use cvws::{ActorClass, Changed, NodeRef, SUB_DOCS, SubDoc, TextField, Workspace};
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

fn at(branch: &str, path: &str) -> NodeRef {
    NodeRef::new(branch, path.split('.').map(str::to_string).collect())
}

fn typed(
    ws: &mut Workspace,
    sub: SubDoc,
    branch: &str,
    path: &str,
    key: &str,
    range: std::ops::Range<u32>,
    text: &str,
) -> Changed {
    let field = TextField {
        sub,
        branch_path: branch,
        node_path: path,
        key,
    };
    ws.type_text(&field, range, text, ActorClass::Human)
        .expect("typed")
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
        vec![at(CL, "cl-root.body")]
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
    let out = ws.take_outgoing().expect("outgoing");
    assert_eq!(out.len(), 2);
    assert_eq!(
        out.iter().map(|o| o.sub_doc).collect::<Vec<_>>(),
        SUB_DOCS.to_vec()
    );
    assert_eq!(ws.take_outgoing().expect("outgoing").len(), 0);

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
            .contains(&at(CV, "root.item.b1"))
    );
    assert_eq!(
        text_of(&peer, SubDoc::CvProfile, CV, "root.item.b1"),
        "Built things"
    );
}

#[test]
fn a_mixed_batch_applies_to_both_sub_docs_or_to_neither() {
    let mut ws = seeded();
    ws.take_outgoing().expect("outgoing");
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
        ws.take_outgoing().expect("outgoing").is_empty(),
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
    ws.take_outgoing().expect("outgoing");
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
        vec![at(CV, "root.item.b1")]
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
    assert_eq!(ws.take_outgoing().expect("outgoing").len(), 0);

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
    let fanout = server.take_outgoing().expect("outgoing").remove(0);

    // The client receives them: the edit appears exactly once ("great", not doubled).
    let applied = ws.receive(&fanout.update, None).expect("receive");
    assert_eq!(
        applied.of(SubDoc::CvProfile).expect("changed").nodes,
        vec![at(CV, "root.item.b1")]
    );
    assert_eq!(
        text_of(&ws, SubDoc::CvProfile, CV, "root.item.b1"),
        "Built great things"
    );
    assert!(
        ws.take_outgoing().expect("outgoing").is_empty(),
        "received updates are not echoed"
    );
}

#[test]
fn a_frame_without_a_sub_doc_tag_is_offered_to_every_sub_doc() {
    let mut ws = seeded();
    ws.take_outgoing().expect("outgoing");
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
    typed(
        &mut other,
        SubDoc::ClProfile,
        CL,
        "cl-root.body",
        "text",
        0..0,
        "Hi. ",
    );
    let update = other.take_outgoing().expect("outgoing").remove(0).update;
    let applied = ws.receive(&update, None).expect("offered");
    assert_eq!(applied.changed.len(), 1);
    assert_eq!(
        applied.of(SubDoc::ClProfile).expect("cl").nodes,
        vec![at(CL, "cl-root.body")]
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
    ws.take_outgoing().expect("outgoing");
    let mut other = Workspace::open(&[(SubDoc::CvProfile, &ws.encode_state(SubDoc::CvProfile))])
        .expect("other");
    typed(
        &mut other,
        SubDoc::CvProfile,
        CV,
        "root.item.b1",
        "text",
        0..0,
        ">",
    );
    let update = other.take_outgoing().expect("outgoing").remove(0).update;
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
    typed(
        &mut ws,
        SubDoc::CvProfile,
        CV,
        "root.item.b1",
        "text",
        6..12,
        "systems",
    );
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

/// A peer workspace opened from the same stored state, with a client id of its own.
fn peer_of(ws: &Workspace) -> Workspace {
    let states: Vec<(SubDoc, Vec<u8>)> =
        SUB_DOCS.iter().map(|s| (*s, ws.encode_state(*s))).collect();
    let borrowed: Vec<(SubDoc, &[u8])> = states.iter().map(|(s, b)| (*s, b.as_slice())).collect();
    Workspace::open(&borrowed).expect("peer")
}

fn frame_of(peer: &mut Workspace) -> Vec<u8> {
    let mut out = peer.take_outgoing().expect("outgoing");
    assert_eq!(out.len(), 1, "one sub-document edited");
    out.remove(0).update
}

fn quiet(ws: &Workspace) {
    for sub in SUB_DOCS {
        assert!(!ws.has_pending(sub), "{sub:?} holds pending updates");
    }
    assert_eq!(
        ws.unrouted(),
        0,
        "frames are still waiting for a sub-document"
    );
}

#[test]
fn frames_that_arrive_out_of_order_are_held_then_applied() {
    let mut ws = seeded();
    ws.take_outgoing().expect("outgoing");
    let mut peer = peer_of(&ws);
    typed(
        &mut peer,
        SubDoc::CvProfile,
        CV,
        "root.item.b1",
        "text",
        0..0,
        "A",
    );
    let first = frame_of(&mut peer);
    typed(
        &mut peer,
        SubDoc::CvProfile,
        CV,
        "root.item.b1",
        "text",
        1..1,
        "B",
    );
    let second = frame_of(&mut peer);

    // The second frame comes first: no sub-document can take it yet. It is held, not dropped.
    let held = ws.receive(&second, None).expect("held");
    assert!(held.is_empty());
    assert_eq!(held.unrouted, 1);
    assert_eq!(
        text_of(&ws, SubDoc::CvProfile, CV, "root.item.b1"),
        "Built things"
    );

    // The first frame finds its sub-document, and the held one follows it.
    let applied = ws.receive(&first, None).expect("first");
    assert_eq!(applied.unrouted, 0);
    assert_eq!(
        applied.of(SubDoc::CvProfile).expect("cv").nodes,
        vec![at(CV, "root.item.b1")]
    );
    assert_eq!(
        text_of(&ws, SubDoc::CvProfile, CV, "root.item.b1"),
        "ABBuilt things"
    );
    quiet(&ws);
    assert!(
        ws.take_outgoing().expect("outgoing").is_empty(),
        "received frames are not echoed"
    );
}

#[test]
fn a_frame_that_changes_nothing_visible_is_still_applied() {
    let mut ws = seeded();
    ws.take_outgoing().expect("outgoing");
    let mut peer = peer_of(&ws);
    // Typed and deleted again: the text ends up as it was.
    typed(
        &mut peer,
        SubDoc::CvProfile,
        CV,
        "root.item.b1",
        "text",
        0..0,
        "zz",
    );
    typed(
        &mut peer,
        SubDoc::CvProfile,
        CV,
        "root.item.b1",
        "text",
        0..2,
        "",
    );
    let no_op = frame_of(&mut peer);
    typed(
        &mut peer,
        SubDoc::CvProfile,
        CV,
        "root.item.b1",
        "text",
        0..0,
        ">",
    );
    let next = frame_of(&mut peer);

    ws.receive(&no_op, None).expect("no-op frame");
    assert_eq!(
        text_of(&ws, SubDoc::CvProfile, CV, "root.item.b1"),
        "Built things"
    );
    // Had the no-op frame been dropped, everything this client sends next would wait forever.
    let applied = ws.receive(&next, None).expect("next frame");
    assert_eq!(
        applied.of(SubDoc::CvProfile).expect("cv").nodes,
        vec![at(CV, "root.item.b1")]
    );
    assert_eq!(
        text_of(&ws, SubDoc::CvProfile, CV, "root.item.b1"),
        ">Built things"
    );
    quiet(&ws);
}

#[test]
fn a_duplicate_frame_changes_nothing_and_leaves_nothing_behind() {
    let mut ws = seeded();
    ws.take_outgoing().expect("outgoing");
    let mut peer = peer_of(&ws);
    typed(
        &mut peer,
        SubDoc::CvProfile,
        CV,
        "root.item.b1",
        "text",
        0..0,
        "A",
    );
    let frame = frame_of(&mut peer);
    assert!(!ws.receive(&frame, None).expect("first").is_empty());
    let again = ws.receive(&frame, None).expect("again");
    assert!(again.is_empty());
    assert_eq!(again.unrouted, 0);
    assert_eq!(
        text_of(&ws, SubDoc::CvProfile, CV, "root.item.b1"),
        "ABuilt things"
    );
    quiet(&ws);
}

#[test]
fn an_untagged_frame_reaches_only_its_own_sub_doc() {
    for (target, branch, path, other) in [
        (SubDoc::CvProfile, CV, "root.item.b1", SubDoc::ClProfile),
        (SubDoc::ClProfile, CL, "cl-root.body", SubDoc::CvProfile),
    ] {
        let mut ws = seeded();
        ws.take_outgoing().expect("outgoing");
        let untouched = ws.encode_state(other);
        let mut peer = peer_of(&ws);
        typed(&mut peer, target, branch, path, "text", 0..0, "!");
        let frame = frame_of(&mut peer);
        let applied = ws.receive(&frame, None).expect("untagged");
        assert_eq!(applied.changed.len(), 1, "{target:?}");
        assert!(applied.of(target).is_some(), "{target:?}");
        assert_eq!(
            ws.encode_state(other),
            untouched,
            "{other:?} must not change"
        );
        quiet(&ws);
        // The next frame of the same client is routed by the client it now knows.
        typed(&mut peer, target, branch, path, "text", 0..0, "?");
        let next = frame_of(&mut peer);
        assert!(
            ws.receive(&next, None)
                .expect("known client")
                .of(target)
                .is_some()
        );
        quiet(&ws);
    }
}

#[test]
fn a_frame_for_a_root_that_is_not_ours_is_dropped_on_purpose() {
    let mut ws = seeded();
    ws.take_outgoing().expect("outgoing");
    let before: Vec<Vec<u8>> = SUB_DOCS.iter().map(|s| ws.encode_state(*s)).collect();
    for root in ["jobs", "settings", "layout"] {
        let foreign = YDoc::new();
        let map = foreign.get_or_insert_map(root);
        {
            let mut txn = foreign.transact_mut();
            map.insert(&mut txn, "k", "v");
        }
        let bytes = foreign
            .transact()
            .encode_state_as_update_v1(&yrs::StateVector::default());
        let applied = ws.receive(&bytes, None).expect("offered");
        assert!(applied.is_empty(), "{root}");
        assert_eq!(
            applied.foreign, 1,
            "{root}: dropped on purpose, and counted"
        );
        assert_eq!(applied.unrouted, 0);
    }
    for (i, sub) in SUB_DOCS.iter().enumerate() {
        assert_eq!(
            ws.encode_state(*sub),
            before[i],
            "{sub:?} kept nothing foreign"
        );
    }
    quiet(&ws);
}

#[test]
fn prepare_writes_the_ids_the_engine_would_generate_into_the_envelope() {
    let mut ws = seeded();
    ws.take_outgoing().expect("outgoing");
    let create = json!({ "kind": "node.create", "branch_path": CV, "parent_path": "root.item",
        "node": { "kind": "bullet", "tier": "HYBRID", "data": { "text": "new" } } });
    let prepared = ws.prepare(&create, ActorClass::Human).expect("prepared");
    let id = prepared.envelope["node"]["id"]
        .as_str()
        .expect("stamped id")
        .to_string();
    assert_ne!(id.len(), 0);
    assert!(
        create["node"]["id"].is_null(),
        "the caller's op is not modified"
    );
    assert_eq!(
        prepared
            .would_change
            .of(SubDoc::CvProfile)
            .expect("would")
            .nodes,
        vec![at(CV, "root.item"), at(CV, &format!("root.item.{id}"))]
    );

    let duplicate = json!({ "kind": "cvl.duplicate_as_variant", "branch_path": CV, "node_path": "root.item.b1" });
    let prepared = ws.prepare(&duplicate, ActorClass::Human).expect("prepared");
    assert!(
        prepared.envelope["new_id"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    assert!(
        prepared.envelope["new_wrapper_id"]
            .as_str()
            .is_some_and(|s| s.starts_with("vp-"))
    );

    // Ids inside a batch are stamped too, and the server that applies the envelope makes the same node.
    let batch = json!({ "kind": "batch", "ops": [{ "op": create }] });
    let prepared = ws.prepare(&batch, ActorClass::Human).expect("prepared");
    let stamped = prepared.envelope["ops"][0]["op"]["node"]["id"]
        .as_str()
        .expect("stamped")
        .to_string();
    let mut server = peer_of(&ws);
    server
        .apply(&prepared.envelope, ActorClass::Human)
        .expect("server applies");
    assert_eq!(
        server
            .view(SubDoc::CvProfile, CV, &format!("root.item.{stamped}"))
            .expect("same id")["id"],
        stamped.as_str()
    );
}

#[test]
fn type_text_through_the_workspace_protects_fixed_nodes_and_refuses_cleanly() {
    let mut ws = seeded();
    ws.apply(
        &json!({ "kind": "node.update", "branch_path": CV, "node_path": "root.item.b1", "patch": { "tier": "FIXED" } }),
        ActorClass::Human,
    )
    .expect("fix it");
    ws.take_outgoing().expect("outgoing");
    let field = TextField {
        sub: SubDoc::CvProfile,
        branch_path: CV,
        node_path: "root.item.b1",
        key: "text",
    };
    let err = ws
        .type_text(&field, 0..0, "x", ActorClass::Agent)
        .expect_err("protected");
    assert!(err.message().starts_with("tier_protected: agent"), "{err}");
    let err = ws
        .type_text(&field, 0..999, "x", ActorClass::Human)
        .expect_err("range");
    assert!(err.message().contains("outside the text"), "{err}");
    assert_eq!(
        text_of(&ws, SubDoc::CvProfile, CV, "root.item.b1"),
        "Built things"
    );
    assert!(
        ws.take_outgoing().expect("outgoing").is_empty(),
        "refused calls emit nothing"
    );
}

// ---------------------------------------------------------------------------
// Frame attribution: no fork, no pollution, no silent loss
// ---------------------------------------------------------------------------

use yrs::{Doc as PlainDoc, MapPrelim, Out, StateVector, TextPrelim, TextRef, Update};

fn bytes_since(doc: &PlainDoc, since: &StateVector) -> Vec<u8> {
    doc.transact().encode_state_as_update_v1(since)
}

fn full_state(doc: &PlainDoc) -> Vec<u8> {
    bytes_since(doc, &StateVector::default())
}

/// A foreign sub-document: a `settings` root holding a profile map with a text.
fn settings_doc() -> (PlainDoc, TextRef) {
    use yrs::Map as _;
    let doc = PlainDoc::new();
    let settings = doc.get_or_insert_map("settings");
    let text = {
        let mut txn = doc.transact_mut();
        let profile = settings.insert(&mut txn, "profile", MapPrelim::default());
        profile.insert(&mut txn, "narrative", TextPrelim::new("hello"))
    };
    (doc, text)
}

fn doc_from(state: &[u8]) -> (PlainDoc, TextRef) {
    use yrs::Map as _;
    use yrs::updates::decoder::Decode;
    let doc = PlainDoc::new();
    let settings = doc.get_or_insert_map("settings");
    doc.transact_mut()
        .apply_update(Update::decode_v1(state).expect("state"))
        .expect("applies");
    let text = {
        let txn = doc.transact();
        let Some(Out::YMap(profile)) = settings.get(&txn, "profile") else {
            panic!("profile map")
        };
        let Some(Out::YText(text)) = profile.get(&txn, "narrative") else {
            panic!("narrative text")
        };
        text
    };
    (doc, text)
}

fn keystroke(doc: &PlainDoc, text: &TextRef, at: u32, s: &str) -> Vec<u8> {
    use yrs::Text as _;
    let before = doc.transact().state_vector();
    text.insert(&mut doc.transact_mut(), at, s);
    bytes_since(doc, &before)
}

#[test]
fn foreign_keystrokes_never_fill_or_block_the_buffer() {
    let mut ws = seeded();
    ws.take_outgoing().expect("outgoing");
    let untouched: Vec<Vec<u8>> = SUB_DOCS.iter().map(|s| ws.encode_state(*s)).collect();
    // The settings sub-document reaches us once as a whole: its clients become known as foreign.
    let (author, _) = settings_doc();
    let first = ws
        .receive(&full_state(&author), None)
        .expect("settings state");
    assert_eq!((first.foreign, first.unrouted), (1, 0));
    // Another tab types into it, over and over: nested frames that name no root.
    let (typist, text) = doc_from(&full_state(&author));
    for i in 0..400 {
        let applied = ws
            .receive(&keystroke(&typist, &text, 0, "x"), None)
            .expect("keystroke");
        assert_eq!(
            (applied.foreign, applied.unrouted, applied.dropped),
            (1, 0, 0),
            "frame {i}"
        );
    }
    assert_eq!(ws.stats().foreign, 401);
    assert_eq!(ws.unrouted(), 0);
    assert!(!ws.take_resync(), "dropping foreign frames needs no resync");
    for (i, sub) in SUB_DOCS.iter().enumerate() {
        assert_eq!(
            ws.encode_state(*sub),
            untouched[i],
            "{sub:?} kept nothing foreign"
        );
    }
    // Profile traffic is not blocked behind any of it.
    let mut peer = peer_of(&ws);
    typed(
        &mut peer,
        SubDoc::CvProfile,
        CV,
        "root.item.b1",
        "text",
        0..0,
        "ok ",
    );
    let applied = ws
        .receive(&frame_of(&mut peer), None)
        .expect("profile frame");
    assert!(applied.of(SubDoc::CvProfile).is_some());
    quiet(&ws);
}

#[test]
fn frames_nobody_claims_are_held_in_a_bounded_buffer_that_drops_the_oldest() {
    let mut ws = seeded();
    ws.take_outgoing().expect("outgoing");
    // 300 tabs type into a foreign document whose state this workspace has never seen.
    let (author, _) = settings_doc();
    let state = full_state(&author);
    let mut total_dropped = 0;
    for i in 0..300 {
        let (tab, text) = doc_from(&state);
        let applied = ws
            .receive(&keystroke(&tab, &text, 0, "x"), None)
            .expect("held");
        assert_eq!(
            applied.foreign, 0,
            "frame {i}: nothing says it is foreign yet"
        );
        total_dropped += applied.dropped;
        assert_eq!(applied.resync, applied.dropped > 0);
    }
    assert_eq!(ws.unrouted(), 256, "bounded");
    assert_eq!(total_dropped, 44, "every drop is counted");
    assert_eq!(ws.stats().dropped, 44);
    assert!(ws.take_resync(), "dropping raises the resync flag");
    assert!(!ws.take_resync(), "and the flag is taken once");

    // The buffer never refuses and never blocks: a profile frame still goes through.
    let mut peer = peer_of(&ws);
    typed(
        &mut peer,
        SubDoc::CvProfile,
        CV,
        "root.item.b1",
        "text",
        0..0,
        "ok ",
    );
    assert!(
        ws.receive(&frame_of(&mut peer), None)
            .expect("profile")
            .of(SubDoc::CvProfile)
            .is_some()
    );

    // The foreign state arrives at last: everything held turns out to be foreign and is let go.
    let applied = ws.receive(&state, None).expect("foreign state");
    assert_eq!(applied.foreign, 257);
    assert_eq!(ws.unrouted(), 0);
    quiet(&ws);
}

#[test]
fn a_frame_that_mixes_sub_docs_is_never_applied_and_pollutes_nothing() {
    let mut ws = seeded();
    ws.take_outgoing().expect("outgoing");
    let untouched: Vec<Vec<u8>> = SUB_DOCS.iter().map(|s| ws.encode_state(*s)).collect();

    // Both profile roots in one frame, as a sync step 2 over a merged document would carry.
    let mut merged = ccvl_core::doc::Doc::new();
    for (kind, tree) in [
        ("cv_profile.replace", cv_tree()),
        ("cl_profile.replace", cl_tree()),
    ] {
        merged.apply_op(&replace(kind, &tree)).expect("seed");
    }
    // A profile root together with a foreign one.
    let profile_and_foreign = PlainDoc::new();
    {
        use yrs::Map as _;
        let cv = profile_and_foreign.get_or_insert_map("cv_profile");
        let settings = profile_and_foreign.get_or_insert_map("settings");
        let mut txn = profile_and_foreign.transact_mut();
        cv.insert(&mut txn, "en", MapPrelim::default());
        settings.insert(&mut txn, "theme", "dark");
    }
    for frame in [merged.encode_state(), full_state(&profile_and_foreign)] {
        let applied = ws.receive(&frame, None).expect("mixed");
        assert_eq!(
            (applied.mixed, applied.foreign, applied.unrouted),
            (1, 0, 0)
        );
        assert!(applied.is_empty());
        assert!(applied.resync);
        for (i, sub) in SUB_DOCS.iter().enumerate() {
            assert_eq!(ws.encode_state(*sub), untouched[i], "{sub:?} was polluted");
        }
    }
    assert!(ws.take_resync());
    assert_eq!(ws.stats().mixed, 2);
    quiet(&ws);
    // The documents keep working for ordinary frames afterwards.
    let mut peer = peer_of(&ws);
    typed(
        &mut peer,
        SubDoc::ClProfile,
        CL,
        "cl-root.body",
        "text",
        0..0,
        "Hi ",
    );
    assert!(
        ws.receive(&frame_of(&mut peer), None)
            .expect("frame")
            .of(SubDoc::ClProfile)
            .is_some()
    );
}

#[test]
fn a_dependent_frame_that_arrives_first_waits_for_its_dependency_across_two_clients() {
    let mut ws = seeded();
    ws.take_outgoing().expect("outgoing");
    let mut a = peer_of(&ws);
    typed(
        &mut a,
        SubDoc::CvProfile,
        CV,
        "root.item.b1",
        "text",
        0..0,
        "A",
    );
    let from_a = frame_of(&mut a);
    // B has seen A's frame and types after it: B's frame builds on A's.
    let mut b = peer_of(&ws);
    b.receive(&from_a, Some(SubDoc::CvProfile))
        .expect("b sees a");
    typed(
        &mut b,
        SubDoc::CvProfile,
        CV,
        "root.item.b1",
        "text",
        1..1,
        "B",
    );
    let from_b = frame_of(&mut b);

    ws.receive(&from_b, None).expect("b first");
    assert_eq!(
        text_of(&ws, SubDoc::CvProfile, CV, "root.item.b1"),
        "Built things"
    );
    let applied = ws.receive(&from_a, None).expect("then a");
    assert!(applied.of(SubDoc::CvProfile).is_some());
    assert_eq!(
        text_of(&ws, SubDoc::CvProfile, CV, "root.item.b1"),
        "ABBuilt things"
    );
    quiet(&ws);
}

#[test]
fn a_hinted_receive_retries_held_frames_too() {
    let mut ws = seeded();
    ws.take_outgoing().expect("outgoing");
    // X creates a node; Y, who has seen it, types into it. Neither is known here.
    let mut x = peer_of(&ws);
    x.apply(
        &json!({ "kind": "node.create", "branch_path": CV, "parent_path": "root.item",
                 "node": node("b9", "bullet", json!({ "text": "fresh" }), vec![]) }),
        ActorClass::Human,
    )
    .expect("x creates");
    let created = frame_of(&mut x);
    let mut y = peer_of(&ws);
    y.receive(&created, Some(SubDoc::CvProfile))
        .expect("y sees it");
    typed(
        &mut y,
        SubDoc::CvProfile,
        CV,
        "root.item.b9",
        "text",
        0..0,
        "very ",
    );
    let typing = frame_of(&mut y);

    let held = ws.receive(&typing, None).expect("held");
    assert_eq!(held.unrouted, 1);
    // A tagged cold load closes the gap and the held frame follows without another untagged frame.
    let applied = ws
        .receive(&created, Some(SubDoc::CvProfile))
        .expect("hinted");
    assert_eq!(applied.unrouted, 0);
    assert_eq!(
        text_of(&ws, SubDoc::CvProfile, CV, "root.item.b9"),
        "very fresh"
    );
    quiet(&ws);
}

#[test]
fn oversized_frames_are_refused_before_they_are_decoded() {
    let mut ws = seeded();
    let err = ws
        .receive(&vec![0_u8; cvws::MAX_FRAME_BYTES + 1], None)
        .expect_err("too large");
    assert!(err.message().contains("larger than"), "{err}");
    assert!(
        ws.receive(&[0xff, 0xff, 0xff], None).is_err(),
        "undecodable frames are errors, not held"
    );
    assert_eq!(ws.unrouted(), 0);
}

#[test]
fn translate_is_routed_like_the_web_to_cv_profile() {
    let mut ws = seeded();
    let translate = json!({ "kind": "translate",
        "source_path": "cl_profile.en.tree.cl-root.body", "target_path": "cl_profile.de.tree.cl-root.body",
        "source_lang": "en", "target_lang": "de",
        "translated_node": node("body", "body", json!({ "archetype": "text", "text": "Liebes Team" }), vec![]) });
    // The cv-profile sub-document does not own the cover letter: prepare refuses what the server refuses.
    assert!(ws.prepare(&translate, ActorClass::Human).is_err());
    assert!(ws.apply(&translate, ActorClass::Human).is_err());
}

#[test]
fn bytes_that_could_not_be_sent_can_be_given_back() {
    let mut ws = seeded();
    let first = ws.take_outgoing().expect("outgoing");
    assert_eq!(first.len(), 2);
    ws.restore_outgoing(first.clone());
    assert_eq!(
        ws.take_outgoing().expect("again"),
        first,
        "same bytes, same order"
    );
    assert_eq!(ws.take_outgoing().expect("empty").len(), 0);
}

#[test]
fn a_node_is_viewed_by_the_location_a_change_reported() {
    let mut ws = seeded();
    let applied = ws
        .apply(
            &json!({ "kind": "node.update", "branch_path": CL, "node_path": "cl-root.body", "patch": { "tier": "OPEN" } }),
            ActorClass::Human,
        )
        .expect("apply");
    let at = &applied.of(SubDoc::ClProfile).expect("cl").nodes[0];
    assert_eq!(ws.view_ref(at).expect("view")["tier"], "OPEN");
    assert!(
        ws.view_ref(&NodeRef::new("jobs.en", vec!["x".into()]))
            .is_err()
    );
}

#[test]
fn a_sub_doc_never_holds_or_reports_the_other_root() {
    let roots_of = |ws: &Workspace, sub: SubDoc| -> Vec<String> {
        ccvl_core::doc::inspect_update(&ws.encode_state(sub))
            .expect("state")
            .roots
            .into_iter()
            .collect()
    };
    let mut ws = seeded();
    ws.take_outgoing().expect("outgoing");
    let mut merged = ccvl_core::doc::Doc::new();
    for (kind, tree) in [
        ("cv_profile.replace", cv_tree()),
        ("cl_profile.replace", cl_tree()),
    ] {
        merged.apply_op(&replace(kind, &tree)).expect("seed");
    }
    let (settings, _) = settings_doc();
    let frames = [merged.encode_state(), full_state(&settings)];
    let mut reports: Vec<cvws::Applied> = Vec::new();
    for frame in &frames {
        reports.push(ws.receive(frame, None).expect("untagged"));
        for sub in SUB_DOCS {
            reports.push(ws.receive(frame, Some(sub)).expect("hinted"));
        }
    }
    // A frame for the cover letter, hinted to the CV sub-document by mistake.
    let mut cl_peer = peer_of(&ws);
    typed(
        &mut cl_peer,
        SubDoc::ClProfile,
        CL,
        "cl-root.body",
        "text",
        0..0,
        "x",
    );
    let stray = frame_of(&mut cl_peer);
    reports.push(
        ws.receive(&stray, Some(SubDoc::CvProfile))
            .expect("hinted wrongly"),
    );

    for sub in SUB_DOCS {
        assert_eq!(
            roots_of(&ws, sub),
            vec![sub.root().to_string()],
            "{sub:?} holds a foreign root"
        );
        assert!(!ws.has_pending(sub), "{sub:?}");
    }
    for report in &reports {
        for (sub, changed) in &report.changed {
            assert!(
                changed
                    .nodes
                    .iter()
                    .all(|n| n.branch.starts_with(sub.root())),
                "{sub:?} reported {changed:?}"
            );
        }
    }
    assert!(reports.iter().any(|r| r.mixed > 0));
    assert!(reports.iter().any(|r| r.foreign > 0));
    // Ops cannot reach the other root either.
    let cross = json!({ "kind": "batch", "ops": [
        { "subDoc": "cv-profile", "op": { "kind": "node.update", "branch_path": CL, "node_path": "cl-root.body", "patch": {} } } ] });
    assert!(ws.apply(&cross, ActorClass::Human).is_err());
}
