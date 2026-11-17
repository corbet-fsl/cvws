//! Workspace bundle (charter section 8): a full-fidelity, self-describing export of one workspace.
//!
//! A bundle holds a manifest, every document (the envelope with its opaque payload, Yjs state vector
//! and clock), the retained earlier revisions, the Yjs op logs, the receipts and the assets. Nothing
//! is parsed or normalised: payloads, metadata text and update bytes travel as they are stored, so
//! `export -> import -> export` is byte-identical on every backend.
//!
//! Unknown fields: top-level fields a newer writer added are returned by [`import_bundle`] in
//! [`Imported::extra`] and re-emitted by [`export_bundle_with`]; unknown fields inside a document's
//! `metadata` are part of the opaque metadata text and are kept. An entry (record, op, receipt) with a
//! field this version does not know is refused rather than silently dropped.

use std::fmt::Write as _;

use cvfs::{Dump, Error, Op, Receipt, Record, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::envelope::content_hash;
use crate::store::Store;

/// The bundle format identifier.
pub const FORMAT: &str = "cvws.bundle/v1";

/// What a bundle contains, plus the hash that seals it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub workspace: String,
    pub documents: u64,
    pub retained: u64,
    pub ops: u64,
    pub receipts: u64,
    pub assets: u64,
    /// `sha256:<hex>` of the canonical JSON of the sections after the manifest.
    pub content_hash: String,
}

/// The result of an import: the manifest as read and the unknown top-level fields.
#[derive(Clone, Debug, PartialEq)]
pub struct Imported {
    pub manifest: Manifest,
    pub extra: Map<String, Value>,
}

#[derive(Serialize, Deserialize)]
struct KeyedOp {
    key: String,
    op: Op,
}

#[derive(Serialize, Deserialize)]
struct Asset {
    path: String,
    bytes: String,
}

/// The sections that the manifest hash covers, in canonical order.
#[derive(Serialize, Deserialize)]
struct Body {
    records: Vec<Record>,
    retained: Vec<Record>,
    ops: Vec<KeyedOp>,
    receipts: Vec<Receipt>,
    assets: Vec<Asset>,
}

#[derive(Serialize)]
struct Out<'a> {
    manifest: &'a Manifest,
    #[serde(flatten)]
    body: &'a Body,
    #[serde(flatten)]
    extra: &'a Map<String, Value>,
}

#[derive(Deserialize)]
struct In {
    manifest: Manifest,
    #[serde(flatten)]
    body: Body,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

fn invalid(what: impl Into<String>) -> Error {
    Error::Invalid(what.into())
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn unhex(text: &str) -> Result<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return Err(invalid("asset bytes are not hex"));
    }
    (0..text.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&text[i..i + 2], 16).map_err(|_| invalid("asset bytes are not hex"))
        })
        .collect()
}

fn body_hash(body: &Body) -> Result<String> {
    let bytes = serde_json::to_vec(body).map_err(|e| invalid(e.to_string()))?;
    Ok(content_hash(&bytes))
}

fn body_of(dump: &Dump) -> Body {
    Body {
        records: dump.records.clone(),
        retained: dump.retained.clone(),
        ops: dump
            .ops
            .iter()
            .map(|(key, op)| KeyedOp {
                key: key.clone(),
                op: op.clone(),
            })
            .collect(),
        receipts: dump.receipts.clone(),
        assets: dump
            .assets
            .iter()
            .map(|(path, bytes)| Asset {
                path: path.clone(),
                bytes: hex(bytes),
            })
            .collect(),
    }
}

/// Export one workspace as a bundle.
pub fn export_bundle(store: &dyn Store, ws: &str) -> Result<Vec<u8>> {
    export_bundle_with(store, ws, &Map::new())
}

/// Export one workspace and re-emit unknown top-level fields (from [`Imported::extra`]).
pub fn export_bundle_with(
    store: &dyn Store,
    ws: &str,
    extra: &Map<String, Value>,
) -> Result<Vec<u8>> {
    let body = body_of(&store.dump(ws)?);
    let manifest = Manifest {
        format: FORMAT.into(),
        workspace: ws.into(),
        documents: body.records.len() as u64,
        retained: body.retained.len() as u64,
        ops: body.ops.len() as u64,
        receipts: body.receipts.len() as u64,
        assets: body.assets.len() as u64,
        content_hash: body_hash(&body)?,
    };
    serde_json::to_vec(&Out {
        manifest: &manifest,
        body: &body,
        extra,
    })
    .map_err(|e| invalid(e.to_string()))
}

const ENTRY_FIELDS: &[(&str, &[&str])] = &[
    (
        "records",
        &[
            "schema_id",
            "key",
            "kind",
            "revision",
            "content_hash",
            "bytes",
            "metadata",
            "yjs",
            "origin",
        ],
    ),
    (
        "retained",
        &[
            "schema_id",
            "key",
            "kind",
            "revision",
            "content_hash",
            "bytes",
            "metadata",
            "yjs",
            "origin",
        ],
    ),
    ("ops", &["key", "op"]),
    (
        "receipts",
        &["seq", "key", "revision", "clock", "content_hash", "at_ms"],
    ),
    ("assets", &["path", "bytes"]),
];

/// Refuse entries that carry a field this version would drop.
fn check_entries(root: &Value) -> Result<()> {
    for (section, known) in ENTRY_FIELDS {
        let Some(list) = root.get(*section).and_then(Value::as_array) else {
            continue;
        };
        for entry in list {
            let Some(obj) = entry.as_object() else {
                continue;
            };
            if let Some(field) = obj.keys().find(|k| !known.contains(&k.as_str())) {
                return Err(invalid(format!(
                    "{section}: unknown field \"{field}\" would be lost; upgrade cvws to import this bundle"
                )));
            }
        }
    }
    Ok(())
}

/// Import a bundle into an empty workspace of `store`. The workspace is read back afterwards and
/// compared with the bundle; a difference is an error. `ws` may differ from the bundle's workspace
/// (moving a workspace).
pub fn import_bundle(store: &dyn Store, ws: &str, bytes: &[u8]) -> Result<Imported> {
    let root: Value =
        serde_json::from_slice(bytes).map_err(|e| invalid(format!("not a bundle: {e}")))?;
    check_entries(&root)?;
    let input: In =
        serde_json::from_value(root).map_err(|e| invalid(format!("not a bundle: {e}")))?;
    let In {
        manifest,
        body,
        extra,
    } = input;
    if manifest.format != FORMAT {
        return Err(invalid(format!(
            "unsupported bundle format {}",
            manifest.format
        )));
    }
    if body_hash(&body)? != manifest.content_hash {
        return Err(Error::Integrity(
            "the bundle does not match its hash".into(),
        ));
    }
    let counts = (
        body.records.len(),
        body.retained.len(),
        body.ops.len(),
        body.receipts.len(),
        body.assets.len(),
    );
    let declared = (
        manifest.documents,
        manifest.retained,
        manifest.ops,
        manifest.receipts,
        manifest.assets,
    );
    if [counts.0, counts.1, counts.2, counts.3, counts.4].map(|n| n as u64)
        != [declared.0, declared.1, declared.2, declared.3, declared.4]
    {
        return Err(Error::Integrity("the bundle counts do not match".into()));
    }
    for r in body.records.iter().chain(&body.retained) {
        if content_hash(&r.bytes) != r.content_hash {
            return Err(Error::Integrity(format!(
                "{}: hash does not match bytes",
                r.key
            )));
        }
    }
    let dump = Dump {
        records: body.records,
        retained: body.retained,
        ops: body.ops.into_iter().map(|k| (k.key, k.op)).collect(),
        receipts: body.receipts,
        assets: body
            .assets
            .into_iter()
            .map(|a| unhex(&a.bytes).map(|b| (a.path, b)))
            .collect::<Result<_>>()?,
    };
    store.restore(ws, &dump)?;
    if store.dump(ws)? != dump {
        return Err(Error::Integrity(format!(
            "workspace {ws}: the import differs from the bundle"
        )));
    }
    Ok(Imported { manifest, extra })
}
