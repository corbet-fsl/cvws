//! `cvws`: the workspace API over the `ccvl` document engine.
//!
//! A `CareerVector` workspace is partitioned into sub-documents; the profile
//! sub-documents (`cv-profile`, `cl-profile`) hold the CV and cover-letter
//! trees the engine edits. `Workspace` keeps one engine document per profile
//! sub-document and gives a host (a TUI, a CLI, a sync backend) one small
//! surface, with no I/O and no knowledge of what the documents say:
//!
//! | call | what it does |
//! |---|---|
//! | [`Workspace::open`] | load sub-documents from stored Yjs state |
//! | [`Workspace::apply`] | route an op (or a batch) to its sub-documents and apply it |
//! | [`Workspace::receive`] | take update bytes from the network or a store |
//! | [`Workspace::take_outgoing`] | the bytes local edits produced, per sub-document |
//! | [`Workspace::view`] | read one node |
//!
//! Two more calls serve hosts that talk to a server which applies ops itself:
//! [`Workspace::prepare`] builds and validates an op envelope without applying
//! it (the server's bytes come back through `receive`, so the edit is never
//! applied twice), and [`Workspace::type_text`] is the keystroke path.
//!
//! Every call returns the nodes that changed, by location (`NodeRef`: branch and
//! id path), never a bare id: ids repeat across language trees. A caller reads
//! only those nodes.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ops::Range;

pub use ccvl_core::doc::{
    ActorClass, Changed, NodeRef, OP_CATALOG_VERSION, OpError, OutgoingError,
};
use ccvl_core::doc::{Doc, UpdateInfo, inspect_update};
use serde_json::{Map, Value};

/// A profile sub-document of the workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SubDoc {
    CvProfile,
    ClProfile,
}

/// The two sub-documents the engine edits, in the order the host lists them.
pub const SUB_DOCS: [SubDoc; 2] = [SubDoc::CvProfile, SubDoc::ClProfile];

impl SubDoc {
    /// The sub-document name used in op envelopes and on the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CvProfile => "cv-profile",
            Self::ClProfile => "cl-profile",
        }
    }

    /// The Y.Map root holding the languages of this sub-document.
    #[must_use]
    pub const fn root(self) -> &'static str {
        match self {
            Self::CvProfile => "cv_profile",
            Self::ClProfile => "cl_profile",
        }
    }

    /// Parse `cv-profile` or `cl-profile`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "cv-profile" => Some(Self::CvProfile),
            "cl-profile" => Some(Self::ClProfile),
            _ => None,
        }
    }

    fn from_root(root: &str) -> Option<Self> {
        match root {
            "cv_profile" => Some(Self::CvProfile),
            "cl_profile" => Some(Self::ClProfile),
            _ => None,
        }
    }
}

/// What a call changed, per sub-document in `SUB_DOCS` order; sub-documents that
/// did not change are absent. Counters are for this call (held frames that settled
/// during it included).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Applied {
    pub changed: Vec<(SubDoc, Changed)>,
    /// Frames that belong to no sub-document of ours (a `jobs` or `settings`
    /// update): dropped on purpose.
    pub foreign: usize,
    /// Frames that mix sub-documents (more than one profile root, or a profile
    /// root and a foreign one). A frame cannot be split, so none was applied: the
    /// host must resync.
    pub mixed: usize,
    /// Frames lost: pushed out of the full holding buffer, or refused by their
    /// sub-document. The host must resync.
    pub dropped: usize,
    /// Frames still waiting for a sub-document, after this call.
    pub unrouted: usize,
    /// Per-sub-document failures of this call; what applied elsewhere is in `changed`.
    pub failed: Vec<(SubDoc, OpError)>,
    /// True when this call raised the resync flag (see [`Workspace::take_resync`]).
    pub resync: bool,
}

impl Applied {
    /// True when no sub-document changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty()
    }

    /// The change of one sub-document.
    #[must_use]
    pub fn of(&self, sub: SubDoc) -> Option<&Changed> {
        self.changed.iter().find(|(s, _)| *s == sub).map(|(_, c)| c)
    }

    fn merge(&mut self, sub: SubDoc, change: Changed) {
        if change.is_empty() {
            return;
        }
        if let Some((_, existing)) = self.changed.iter_mut().find(|(s, _)| *s == sub) {
            existing.nodes.extend(change.nodes);
            existing.nodes.sort();
            existing.nodes.dedup();
            existing.formats.extend(change.formats);
            existing.formats.sort();
            existing.formats.dedup();
        } else {
            self.changed.push((sub, change));
            self.changed.sort_by_key(|(s, _)| *s);
        }
    }
}

/// An op envelope that was checked against copies of the documents.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Prepared {
    /// The op to send, with an `opId` (kept if the caller set one).
    pub envelope: Value,
    /// The sub-documents it touches.
    pub routes: Vec<SubDoc>,
    /// What applying it would change, per sub-document.
    pub would_change: Applied,
}

/// A prose field of a node: where [`Workspace::type_text`] edits.
#[derive(Clone, Copy, Debug)]
pub struct TextField<'a> {
    pub sub: SubDoc,
    /// `<profile>.<lang>`, for example `cv_profile.en`.
    pub branch_path: &'a str,
    /// Dot-joined id path from the tree root.
    pub node_path: &'a str,
    /// The prose key of the node's `data`: `text` or `heading`.
    pub key: &'a str,
}

/// Update bytes a local edit produced for one sub-document.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Outgoing {
    pub sub_doc: SubDoc,
    pub update: Vec<u8>,
}

impl Outgoing {
    /// Bytes for a sub-document (to hand back with [`Workspace::restore_outgoing`]).
    #[must_use]
    pub fn new(sub_doc: SubDoc, update: Vec<u8>) -> Self {
        Self { sub_doc, update }
    }
}

/// Lifetime counters of a workspace.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Stats {
    pub foreign: usize,
    pub mixed: usize,
    pub dropped: usize,
    pub held: usize,
}

/// The profile sub-documents of one workspace.
pub struct Workspace {
    docs: BTreeMap<SubDoc, Doc>,
    held: VecDeque<Held>,
    foreign_clients: BTreeSet<u64>,
    resync: bool,
    stats: Stats,
}

/// A frame no sub-document claims yet, with what was read from it once.
struct Held {
    bytes: Vec<u8>,
    info: UpdateInfo,
}

/// Frames larger than this are refused before they are decoded: `yrs` decodes plain
/// nested values recursively, so the size of a frame bounds the stack a hostile one
/// can use. Hosts run `receive` on a thread with a stack sized for it (see README).
pub const MAX_FRAME_BYTES: usize = 1 << 20;

/// Frames held for lack of a sub-document; past this the oldest is dropped, counted,
/// and the resync flag is raised.
pub const MAX_HELD: usize = 256;

/// Clients remembered as foreign; past this the memory starts over.
const MAX_FOREIGN_CLIENTS: usize = 8192;

/// Where a frame goes.
#[derive(Clone, Copy)]
enum Route {
    /// An empty frame.
    Nothing,
    /// A frame for roots that are not ours.
    Foreign,
    Ours(SubDoc),
    /// A frame of more than one sub-document.
    Mixed,
    /// A frame whose sub-document is not known yet.
    Unknown,
}

fn refuse<T>(message: impl Into<String>) -> Result<T, OpError> {
    Err(OpError::from_message(message))
}

/// The sub-document a branch path (`<profile>.<lang>`) or a dotted path starting with a profile belongs to.
fn route_path(path: &str) -> Option<SubDoc> {
    SubDoc::from_root(path.split('.').next().unwrap_or(""))
}

/// The sub-document of one non-batch op.
fn route_op(op: &Value) -> Result<SubDoc, OpError> {
    let kind = op
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("undefined");
    let routed = match kind {
        "cl_profile.replace" => Some(SubDoc::ClProfile),
        "yjs.update" => op
            .get("sub_doc")
            .and_then(Value::as_str)
            .and_then(SubDoc::parse),
        // The web routes every translate to cv-profile (it has no branch_path); the
        // engine of that sub-document refuses what is not its own.
        "cv_profile.replace" | "translate" => Some(SubDoc::CvProfile),
        "node.create"
        | "node.update"
        | "node.delete"
        | "node.move"
        | "selection.update"
        | "cvl.duplicate_as_variant"
        | "cvl.break_out"
        | "cvl.promote_sub"
        | "cvl.demote_bullet" => op
            .get("branch_path")
            .and_then(Value::as_str)
            .and_then(route_path),
        _ => None,
    };
    routed.map_or_else(
        || {
            refuse(format!(
                "cvws: op '{kind}' does not target a profile sub-document (cv-profile, cl-profile)"
            ))
        },
        Ok,
    )
}

/// An op split by sub-document: a plain op has one route, a batch one per group.
fn plan(op: &Value) -> Result<Vec<(SubDoc, Value)>, OpError> {
    if op.get("kind").and_then(Value::as_str) != Some("batch") {
        return Ok(vec![(route_op(op)?, op.clone())]);
    }
    let Some(sub_ops) = op.get("ops").and_then(Value::as_array) else {
        return refuse("batch: 'ops' must be an array");
    };
    let mut groups: BTreeMap<SubDoc, Vec<Value>> = BTreeMap::new();
    for entry in sub_ops {
        let Some(inner) = entry.get("op") else {
            return refuse("batch: every sub-op needs an 'op'");
        };
        let by_op = route_op(inner)?;
        if let Some(declared) = entry.get("subDoc").and_then(Value::as_str)
            && SubDoc::parse(declared) != Some(by_op)
        {
            return refuse(format!(
                "batch: sub-op '{}' is declared for sub-doc '{declared}' but targets '{}'",
                inner
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("undefined"),
                by_op.as_str()
            ));
        }
        groups.entry(by_op).or_default().push(entry.clone());
    }
    if groups.is_empty() {
        // An empty batch is a no-op; it needs no document.
        return Ok(Vec::new());
    }
    let mut planned = Vec::with_capacity(groups.len());
    for (sub, entries) in groups {
        let mut group = op.as_object().cloned().unwrap_or_default();
        group.insert("ops".into(), Value::Array(entries));
        planned.push((sub, Value::Object(group)));
    }
    Ok(planned)
}

/// Write the ids the engine would generate into the op, batch sub-ops included.
fn stamp_ids(op: &mut Value, ids: &mut dyn FnMut() -> String) {
    fn missing(value: Option<&Value>) -> bool {
        value.is_none_or(Value::is_null)
    }
    match op.get("kind").and_then(Value::as_str) {
        Some("node.create") => {
            if let Some(node) = op.get_mut("node").and_then(Value::as_object_mut)
                && missing(node.get("id"))
            {
                node.insert("id".into(), Value::String(ids()));
            }
        }
        Some("cvl.duplicate_as_variant") => {
            if let Some(map) = op.as_object_mut() {
                if missing(map.get("new_id")) {
                    map.insert("new_id".into(), Value::String(ids()));
                }
                if missing(map.get("new_wrapper_id")) {
                    let wrapper: String = ids().chars().take(8).collect();
                    map.insert(
                        "new_wrapper_id".into(),
                        Value::String(format!("vp-{wrapper}")),
                    );
                }
            }
        }
        Some("batch") => {
            if let Some(sub_ops) = op.get_mut("ops").and_then(Value::as_array_mut) {
                for entry in sub_ops {
                    if let Some(inner) = entry.get_mut("op") {
                        stamp_ids(inner, ids);
                    }
                }
            }
        }
        _ => {}
    }
}

impl Workspace {
    /// Load the sub-documents from stored Yjs state (update format v1).
    /// A sub-document without stored state starts empty.
    pub fn open(states: &[(SubDoc, &[u8])]) -> Result<Self, OpError> {
        let mut docs = BTreeMap::new();
        for sub in SUB_DOCS {
            let mut doc = Doc::scoped(sub.root()).expect("a profile root");
            if let Some((_, bytes)) = states.iter().find(|(s, _)| *s == sub) {
                doc.receive(bytes)?;
            }
            docs.insert(sub, doc);
        }
        Ok(Self {
            docs,
            held: VecDeque::new(),
            foreign_clients: BTreeSet::new(),
            resync: false,
            stats: Stats::default(),
        })
    }

    fn doc(&self, sub: SubDoc) -> &Doc {
        self.docs.get(&sub).expect("every sub-document is opened")
    }

    fn doc_mut(&mut self, sub: SubDoc) -> &mut Doc {
        self.docs
            .get_mut(&sub)
            .expect("every sub-document is opened")
    }

    /// Apply an op, or a batch, to the sub-documents it targets.
    ///
    /// A batch that spans both sub-documents is checked against copies first, so
    /// it either applies everywhere or nowhere.
    pub fn apply(&mut self, op: &Value, actor: ActorClass) -> Result<Applied, OpError> {
        let planned = plan(op)?;
        if planned.len() > 1 {
            for (sub, group) in &planned {
                self.doc(*sub).dry_run_as(group, actor)?;
            }
        }
        let mut applied = Applied::default();
        for (sub, group) in &planned {
            let changed = self.doc_mut(*sub).apply_op_as(group, actor)?;
            applied.merge(*sub, changed);
        }
        Ok(applied)
    }

    /// Build and validate an op envelope without applying it.
    ///
    /// For a server that applies ops itself and fans out its own canonical
    /// bytes: a local optimistic apply would be applied a second time when those
    /// bytes arrive. Send `envelope`, then feed the server's bytes to
    /// [`Workspace::receive`]. Nothing is applied and nothing becomes outgoing.
    pub fn prepare(&mut self, op: &Value, actor: ActorClass) -> Result<Prepared, OpError> {
        // Ids the engine would generate are generated here and written into the
        // envelope, so the server that applies it produces the same ones.
        let mut stamped = op.clone();
        let doc = self.doc_mut(SubDoc::CvProfile);
        stamp_ids(&mut stamped, &mut || doc.generate_id());
        let planned = plan(&stamped)?;
        let mut would_change = Applied::default();
        let mut routes = Vec::new();
        for (sub, group) in &planned {
            let changed = self.doc(*sub).dry_run_as(group, actor)?;
            routes.push(*sub);
            would_change.merge(*sub, changed);
        }
        let mut envelope: Map<String, Value> = stamped.as_object().cloned().unwrap_or_default();
        if envelope
            .get("opId")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            let id = self.doc_mut(SubDoc::CvProfile).generate_id();
            envelope.insert("opId".into(), Value::String(id));
        }
        Ok(Prepared {
            envelope: Value::Object(envelope),
            routes,
            would_change,
        })
    }

    /// Take update bytes from the network or a store.
    ///
    /// With `hint` the frame goes to that sub-document. Without it (relay frames
    /// carry no sub-document tag) the frame is attributed by structure, never by what
    /// it would change on screen, and never by forking a document:
    ///
    /// * the roots a frame names (`cv_profile`, `cl_profile`, anything else) and the
    ///   clients it depends on (parents, origins, deleted items, its authors' earlier
    ///   history) are read once, by applying the frame to an empty throw-away document;
    /// * a dependency on a client that a sub-document already holds belongs to that
    ///   sub-document (on the web every sub-document has its own client id); one on a
    ///   client seen only in foreign frames is foreign;
    /// * a frame of one sub-document is applied there; if its history is missing, `yrs`
    ///   keeps it pending until the history arrives;
    /// * a frame that is purely foreign (a `jobs` or `settings` edit) is dropped on
    ///   purpose and counted; its clients are remembered, so the nested frames that
    ///   follow are recognised as foreign too;
    /// * a frame that mixes sub-documents cannot be split: it is not applied, counted,
    ///   and the resync flag is raised, so a document is never polluted;
    /// * a frame whose sub-document is not known yet is held in a bounded buffer,
    ///   retried after every `receive`, and pushed out oldest first when the buffer is
    ///   full (counted, resync flag raised).
    ///
    /// A frame larger than [`MAX_FRAME_BYTES`], or one that does not decode, is an error.
    /// Failures of one sub-document do not hide what another applied: see `Applied::failed`.
    pub fn receive(&mut self, update: &[u8], hint: Option<SubDoc>) -> Result<Applied, OpError> {
        if update.len() > MAX_FRAME_BYTES {
            return refuse(format!(
                "cvws: a frame of {} bytes is larger than the {MAX_FRAME_BYTES} byte limit",
                update.len()
            ));
        }
        let mut applied = Applied::default();
        if let Some(sub) = hint {
            self.apply_to(sub, update, &mut applied);
        } else {
            let info = inspect_update(update)?;
            let route = self.classify(&info);
            self.dispatch(route, update.to_vec(), info, &mut applied);
        }
        self.retry_held(&mut applied);
        applied.unrouted = self.held.len();
        self.stats.held = self.held.len();
        Ok(applied)
    }

    /// Frames waiting for a sub-document.
    #[must_use]
    pub fn unrouted(&self) -> usize {
        self.held.len()
    }

    /// Lifetime counters.
    #[must_use]
    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// True once if frames were lost, mixed or refused since the last call: the host
    /// should resync the affected sub-documents (and rate-limit doing so). Clears the flag.
    pub fn take_resync(&mut self) -> bool {
        std::mem::take(&mut self.resync)
    }

    fn raise_resync(&mut self, applied: &mut Applied) {
        self.resync = true;
        applied.resync = true;
    }

    fn apply_to(&mut self, sub: SubDoc, update: &[u8], applied: &mut Applied) {
        match self.doc_mut(sub).receive(update) {
            Ok(changed) => applied.merge(sub, changed),
            Err(e) => {
                applied.failed.push((sub, e));
                applied.dropped += 1;
                self.stats.dropped += 1;
                self.raise_resync(applied);
            }
        }
    }

    fn dispatch(&mut self, route: Route, bytes: Vec<u8>, info: UpdateInfo, applied: &mut Applied) {
        match route {
            Route::Nothing => {}
            Route::Foreign => {
                applied.foreign += 1;
                self.stats.foreign += 1;
                if self.foreign_clients.len() + info.clients.len() > MAX_FOREIGN_CLIENTS {
                    self.foreign_clients.clear();
                }
                self.foreign_clients.extend(info.clients);
            }
            Route::Ours(sub) => self.apply_to(sub, &bytes, applied),
            Route::Mixed => {
                applied.mixed += 1;
                self.stats.mixed += 1;
                self.raise_resync(applied);
            }
            Route::Unknown => {
                if self.held.iter().any(|held| held.bytes == bytes) {
                    return;
                }
                if self.held.len() >= MAX_HELD {
                    self.held.pop_front();
                    applied.dropped += 1;
                    self.stats.dropped += 1;
                    self.raise_resync(applied);
                }
                self.held.push_back(Held { bytes, info });
            }
        }
    }

    /// Re-attribute held frames until none settles. One frame that fails is dropped and
    /// counted; the frames behind it are still tried.
    fn retry_held(&mut self, applied: &mut Applied) {
        loop {
            let mut progressed = false;
            for _ in 0..self.held.len() {
                let Some(frame) = self.held.pop_front() else {
                    break;
                };
                let route = self.classify(&frame.info);
                if matches!(route, Route::Unknown) {
                    self.held.push_back(frame);
                } else {
                    progressed = true;
                    self.dispatch(route, frame.bytes, frame.info, applied);
                }
            }
            if !progressed {
                return;
            }
        }
    }

    /// Which sub-documents a client is known in.
    fn owners_of(&self, client: u64) -> Vec<SubDoc> {
        SUB_DOCS
            .into_iter()
            .filter(|sub| self.doc(*sub).knows_client(client))
            .collect()
    }

    fn classify(&self, info: &UpdateInfo) -> Route {
        if info.empty {
            return Route::Nothing;
        }
        let mut ours: BTreeSet<SubDoc> = SUB_DOCS
            .into_iter()
            .filter(|s| info.roots.contains(s.root()))
            .collect();
        let mut foreign = info.roots.iter().any(|r| SubDoc::from_root(r).is_none());
        for client in info.clients.iter().chain(&info.depends_on) {
            ours.extend(self.owners_of(*client));
            foreign |= self.foreign_clients.contains(client);
        }
        match (ours.len(), foreign) {
            (0, true) => Route::Foreign,
            (0, false) => Route::Unknown,
            (1, false) => Route::Ours(*ours.iter().next().expect("one element")),
            _ => Route::Mixed,
        }
    }

    /// The update bytes local edits produced since the last call, per sub-document.
    /// If one sub-document cannot hand its bytes over, none are taken.
    pub fn take_outgoing(&mut self) -> Result<Vec<Outgoing>, OutgoingError> {
        let mut taken: Vec<Outgoing> = Vec::new();
        for sub in SUB_DOCS {
            match self.doc_mut(sub).take_outgoing() {
                Ok(update) if update.is_empty() => {}
                Ok(update) => taken.push(Outgoing::new(sub, update)),
                Err(e) => {
                    self.restore_outgoing(taken);
                    return Err(e);
                }
            }
        }
        Ok(taken)
    }

    /// Give bytes taken with [`Workspace::take_outgoing`] back, ahead of anything newer,
    /// when the host could not send or store them.
    pub fn restore_outgoing(&mut self, bytes: Vec<Outgoing>) {
        for given_back in bytes {
            self.doc_mut(given_back.sub_doc)
                .restore_outgoing(given_back.update);
        }
    }

    /// One node and its subtree as JSON; `node_path` is the dot-joined id path from the tree root.
    pub fn view(&self, sub: SubDoc, branch_path: &str, node_path: &str) -> Result<Value, OpError> {
        self.doc(sub).view(branch_path, node_path)
    }

    /// One node by its location, as `Applied` reports it (ids may contain dots).
    pub fn view_ref(&self, at: &NodeRef) -> Result<Value, OpError> {
        let sub = route_path(&at.branch).ok_or_else(|| {
            OpError::from_message(format!(
                "cvws: '{}' is not a profile branch (cv_profile.<lang>, cl_profile.<lang>)",
                at.branch
            ))
        })?;
        self.doc(sub).view_ref(at)
    }

    /// Replace `range` (UTF-16 units) of a prose field: the keystroke path. A refused
    /// call changes nothing; a non-human actor cannot edit a `FIXED` node.
    pub fn type_text(
        &mut self,
        field: &TextField<'_>,
        range: Range<u32>,
        replacement: &str,
        actor: ActorClass,
    ) -> Result<Changed, OpError> {
        self.doc_mut(field.sub).type_text(
            field.branch_path,
            field.node_path,
            field.key,
            range,
            replacement,
            actor,
        )
    }

    /// The whole sub-document as one update, for a store to keep.
    #[must_use]
    pub fn encode_state(&self, sub: SubDoc) -> Vec<u8> {
        self.doc(sub).encode_state()
    }

    /// True when a sub-document holds updates that wait for missing history.
    #[must_use]
    pub fn has_pending(&self, sub: SubDoc) -> bool {
        self.doc(sub).has_pending()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_held_frame_that_fails_is_dropped_and_the_frames_behind_it_still_settle() {
        let tree = json!({ "id": "root", "kind": "cv-root", "tier": "FIXED",
            "data": {}, "selection": { "active": ["b"] }, "children": [
                { "id": "b", "kind": "bullet", "tier": "HYBRID", "data": { "text": "words" },
                  "selection": { "active": [] }, "children": [] }] });
        let mut ws = Workspace::open(&[]).expect("open");
        ws.apply(
            &json!({ "kind": "cv_profile.replace", "lang": "en", "tree": tree }),
            ActorClass::Human,
        )
        .expect("seed");
        let seed = inspect_update(&ws.encode_state(SubDoc::CvProfile)).expect("inspect");
        let mut known = UpdateInfo::default();
        known.depends_on = seed.clients;

        let mut peer = Workspace::open(&[(SubDoc::CvProfile, &ws.encode_state(SubDoc::CvProfile))])
            .expect("peer");
        let field = TextField {
            sub: SubDoc::CvProfile,
            branch_path: "cv_profile.en",
            node_path: "root.b",
            key: "text",
        };
        peer.type_text(&field, 0..0, "good ", ActorClass::Human)
            .expect("typed");
        let good = peer.take_outgoing().expect("outgoing").remove(0).update;

        // A frame that cannot be applied sits in front of a good one.
        ws.held.push_back(Held {
            bytes: vec![1, 2, 3],
            info: known.clone(),
        });
        ws.held.push_back(Held {
            bytes: good,
            info: known,
        });
        let mut applied = Applied::default();
        ws.retry_held(&mut applied);
        assert_eq!(applied.dropped, 1, "the bad frame is dropped and counted");
        assert_eq!(applied.failed.len(), 1);
        assert!(applied.resync);
        assert!(
            applied.of(SubDoc::CvProfile).is_some(),
            "the frame behind it still applied"
        );
        assert_eq!(ws.unrouted(), 0);
        assert_eq!(
            ws.view(SubDoc::CvProfile, "cv_profile.en", "root.b")
                .expect("view")["data"]["text"],
            "good words"
        );
    }
}
