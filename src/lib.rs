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
use std::collections::BTreeMap;
use std::ops::Range;

pub use ccvl_core::doc::{ActorClass, Changed, NodeRef, OP_CATALOG_VERSION, OpError};
use ccvl_core::doc::{Doc, inspect_update};
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
/// did not change are absent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub changed: Vec<(SubDoc, Changed)>,
    /// Frames of this call that belong to no sub-document of ours (a `jobs` or
    /// `settings` update) and were dropped on purpose.
    pub foreign: usize,
    /// Frames held back because no sub-document claims them yet; they are retried
    /// after every `receive`. Zero when everything found its place.
    pub unrouted: usize,
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
pub struct Outgoing {
    pub sub_doc: SubDoc,
    pub update: Vec<u8>,
}

/// The profile sub-documents of one workspace.
pub struct Workspace {
    docs: BTreeMap<SubDoc, Doc>,
    unrouted: Vec<Vec<u8>>,
}

/// Frames held back for lack of a sub-document; past this the next one is refused.
const MAX_UNROUTED: usize = 256;

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
        "cv_profile.replace" => Some(SubDoc::CvProfile),
        "cl_profile.replace" => Some(SubDoc::ClProfile),
        "yjs.update" => op
            .get("sub_doc")
            .and_then(Value::as_str)
            .and_then(SubDoc::parse),
        "translate" => op
            .get("source_path")
            .and_then(Value::as_str)
            .and_then(route_path),
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

/// Where a frame without a sub-document tag goes.
enum Route {
    /// An empty frame.
    Nothing,
    /// A frame for a root that is not ours.
    Foreign,
    Ours(Vec<SubDoc>),
    Unrouted,
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
            let doc = match states.iter().find(|(s, _)| *s == sub) {
                Some((_, bytes)) => Doc::load(bytes)?,
                None => Doc::new(),
            };
            docs.insert(sub, doc);
        }
        Ok(Self {
            docs,
            unrouted: Vec::new(),
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
    /// With `hint` the update goes to that sub-document. Without it (relay
    /// frames carry no sub-document tag) the frame is routed by structure, never
    /// by what it would change on screen, so no update is lost:
    ///
    /// 1. a frame that names a root of ours (`cv_profile`, `cl_profile`) goes to
    ///    that sub-document;
    /// 2. a frame that names only other roots (`jobs`, `settings`, ...) is not
    ///    ours and is dropped on purpose (`Applied::foreign`);
    /// 3. otherwise it goes to the sub-document that already knows one of its
    ///    clients (on the web every sub-document has its own client id);
    /// 4. a frame from a client no sub-document has seen goes to the one
    ///    sub-document it integrates into without waiting for history;
    /// 5. a frame no sub-document claims yet is held (`Applied::unrouted`) and
    ///    retried after every `receive`.
    ///
    /// A frame whose history is still missing stays pending in its sub-document
    /// until the history arrives.
    pub fn receive(&mut self, update: &[u8], hint: Option<SubDoc>) -> Result<Applied, OpError> {
        let mut applied = Applied::default();
        if let Some(sub) = hint {
            let changed = self.doc_mut(sub).receive(update)?;
            applied.merge(sub, changed);
            return Ok(applied);
        }
        let settled = self.place(update, &mut applied)?;
        if settled {
            self.retry_unrouted(&mut applied)?;
        }
        applied.unrouted = self.unrouted.len();
        Ok(applied)
    }

    /// Frames waiting for a sub-document.
    #[must_use]
    pub fn unrouted(&self) -> usize {
        self.unrouted.len()
    }

    /// Route one frame and apply it; false when it had to be held back.
    fn place(&mut self, update: &[u8], applied: &mut Applied) -> Result<bool, OpError> {
        match self.route(update)? {
            Route::Nothing => Ok(true),
            Route::Foreign => {
                applied.foreign += 1;
                Ok(true)
            }
            Route::Ours(subs) => {
                for sub in subs {
                    let changed = self.doc_mut(sub).receive(update)?;
                    applied.merge(sub, changed);
                }
                Ok(true)
            }
            Route::Unrouted => {
                if !self.unrouted.iter().any(|held| held == update) {
                    if self.unrouted.len() >= MAX_UNROUTED {
                        return refuse(format!(
                            "cvws: {MAX_UNROUTED} frames are waiting for a sub-document; refusing more"
                        ));
                    }
                    self.unrouted.push(update.to_vec());
                }
                Ok(false)
            }
        }
    }

    fn retry_unrouted(&mut self, applied: &mut Applied) -> Result<(), OpError> {
        loop {
            let held = std::mem::take(&mut self.unrouted);
            let before = held.len();
            for frame in held {
                self.place(&frame, applied)?;
            }
            if self.unrouted.len() >= before {
                return Ok(());
            }
        }
    }

    fn route(&self, update: &[u8]) -> Result<Route, OpError> {
        let info = inspect_update(update)?;
        if info.empty {
            return Ok(Route::Nothing);
        }
        let named: Vec<SubDoc> = SUB_DOCS
            .into_iter()
            .filter(|s| info.roots.contains(s.root()))
            .collect();
        if !named.is_empty() {
            return Ok(Route::Ours(named));
        }
        if !info.roots.is_empty() {
            return Ok(Route::Foreign);
        }
        // Nested structs only: the parents are known somewhere, or not yet.
        let known: Vec<SubDoc> = SUB_DOCS
            .into_iter()
            .filter(|s| info.clients.iter().any(|c| self.doc(*s).knows_client(*c)))
            .collect();
        if !known.is_empty() {
            return Ok(Route::Ours(known));
        }
        let mut accepting = Vec::new();
        for sub in SUB_DOCS {
            let mut probe = self.doc(sub).fork()?;
            probe.receive(update)?;
            if !probe.has_pending() {
                accepting.push(sub);
            }
        }
        if accepting.len() == 1 {
            Ok(Route::Ours(accepting))
        } else {
            Ok(Route::Unrouted)
        }
    }

    /// The update bytes local edits produced since the last call, per sub-document.
    /// If one sub-document cannot hand its bytes over, none are taken.
    pub fn take_outgoing(&mut self) -> Result<Vec<Outgoing>, OpError> {
        let mut taken: Vec<Outgoing> = Vec::new();
        for sub in SUB_DOCS {
            match self.doc_mut(sub).take_outgoing() {
                Ok(update) if update.is_empty() => {}
                Ok(update) => taken.push(Outgoing {
                    sub_doc: sub,
                    update,
                }),
                Err(e) => {
                    for given_back in taken {
                        self.doc_mut(given_back.sub_doc)
                            .restore_outgoing(given_back.update);
                    }
                    return Err(e);
                }
            }
        }
        Ok(taken)
    }

    /// One node and its subtree as JSON; `node_path` is the dot-joined id path from the tree root.
    pub fn view(&self, sub: SubDoc, branch_path: &str, node_path: &str) -> Result<Value, OpError> {
        self.doc(sub).view(branch_path, node_path)
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
