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
//! Every call returns the ids that changed; a caller reads only those.
use std::collections::BTreeMap;
use std::ops::Range;

use ccvl_core::doc::Doc;
pub use ccvl_core::doc::{ActorClass, Changed, OP_CATALOG_VERSION, OpError};
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

/// Changed ids per sub-document, in `SUB_DOCS` order; sub-documents that did not change are absent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub changed: Vec<(SubDoc, Changed)>,
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

/// Update bytes a local edit produced for one sub-document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outgoing {
    pub sub_doc: SubDoc,
    pub update: Vec<u8>,
}

/// The profile sub-documents of one workspace.
pub struct Workspace {
    docs: BTreeMap<SubDoc, Doc>,
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
        Ok(Self { docs })
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
            if !changed.is_empty() {
                applied.changed.push((*sub, changed));
            }
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
        let planned = plan(op)?;
        let mut would_change = Applied::default();
        let mut routes = Vec::new();
        for (sub, group) in &planned {
            let changed = self.doc(*sub).dry_run_as(group, actor)?;
            routes.push(*sub);
            if !changed.is_empty() {
                would_change.changed.push((*sub, changed));
            }
        }
        let mut envelope: Map<String, Value> = op.as_object().cloned().unwrap_or_default();
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
    /// frames carry no sub-document tag) the update is offered to every
    /// sub-document: each one tries it on a copy and keeps it only if its own
    /// profile root changes, so an update for another sub-document leaves no
    /// foreign roots and no pending state behind.
    pub fn receive(&mut self, update: &[u8], hint: Option<SubDoc>) -> Result<Applied, OpError> {
        let mut applied = Applied::default();
        for sub in SUB_DOCS {
            if hint.is_some_and(|h| h != sub) {
                continue;
            }
            if hint.is_none() && !self.offer_changes_projection(sub, update)? {
                continue;
            }
            let changed = self.doc_mut(sub).receive(update)?;
            if !changed.is_empty() {
                applied.changed.push((sub, changed));
            }
        }
        Ok(applied)
    }

    fn offer_changes_projection(&self, sub: SubDoc, update: &[u8]) -> Result<bool, OpError> {
        let doc = self.doc(sub);
        let before = doc.profile_json(sub.root())?;
        let mut probe = doc.fork()?;
        probe.receive(update)?;
        Ok(probe.profile_json(sub.root())? != before)
    }

    /// The update bytes local edits produced since the last call, per sub-document.
    pub fn take_outgoing(&mut self) -> Vec<Outgoing> {
        SUB_DOCS
            .into_iter()
            .filter_map(|sub| {
                let update = self.doc_mut(sub).take_outgoing();
                (!update.is_empty()).then_some(Outgoing {
                    sub_doc: sub,
                    update,
                })
            })
            .collect()
    }

    /// One node and its subtree as JSON; `node_path` is the dot-joined id path from the tree root.
    pub fn view(&self, sub: SubDoc, branch_path: &str, node_path: &str) -> Result<Value, OpError> {
        self.doc(sub).view(branch_path, node_path)
    }

    /// Replace `range` (UTF-16 units) of a prose field: the keystroke path.
    pub fn type_text(
        &mut self,
        sub: SubDoc,
        branch_path: &str,
        node_path: &str,
        key: &str,
        range: Range<u32>,
        replacement: &str,
    ) -> Result<Changed, OpError> {
        self.doc_mut(sub)
            .type_text(branch_path, node_path, key, range, replacement)
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
