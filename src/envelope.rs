//! The document envelope (charter section 6): what a storage backend keeps of every document.
//!
//! The payload is opaque. `cvws` and the backends never parse TOML, Typst, Node JSON or Yjs.

use serde_json::{Map, Value};

pub use cvfs::{Op, Origin, Receipt, Yjs, content_hash};

/// What the payload is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A projected document (Node tree, `StyleDocument`, TUI application).
    Projected,
    /// One Yjs update.
    CrdtUpdate,
    /// A Yjs snapshot.
    CrdtSnapshot,
}

impl Kind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Projected => "projected",
            Self::CrdtUpdate => "crdt-update",
            Self::CrdtSnapshot => "crdt-snapshot",
        }
    }

    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "projected" => Some(Self::Projected),
            "crdt-update" => Some(Self::CrdtUpdate),
            "crdt-snapshot" => Some(Self::CrdtSnapshot),
            _ => None,
        }
    }
}

/// One stored document.
#[derive(Clone, Debug, PartialEq)]
pub struct Envelope {
    pub schema_id: String,
    pub key: String,
    pub kind: Kind,
    /// Starts at 1 and grows by one with every accepted write.
    pub revision: u64,
    /// `sha256:<hex>` of `bytes`.
    pub content_hash: String,
    pub bytes: Vec<u8>,
    /// Free-form, preserved including unknown fields.
    pub metadata: Map<String, Value>,
    /// Present for Yjs sub-documents (web collaboration).
    pub yjs: Option<Yjs>,
    pub origin: Option<Origin>,
}

/// A document before its revision and hash exist.
#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    pub schema_id: String,
    pub key: String,
    pub kind: Kind,
    pub bytes: Vec<u8>,
    pub metadata: Map<String, Value>,
    pub origin: Option<Origin>,
}

impl Draft {
    #[must_use]
    pub fn new(schema_id: &str, key: &str, kind: Kind, bytes: Vec<u8>) -> Self {
        Self {
            schema_id: schema_id.into(),
            key: key.into(),
            kind,
            bytes,
            metadata: Map::new(),
            origin: None,
        }
    }

    /// The envelope this draft becomes at `revision`.
    #[must_use]
    pub fn at(self, revision: u64, yjs: Option<Yjs>) -> Envelope {
        Envelope {
            content_hash: content_hash(&self.bytes),
            schema_id: self.schema_id,
            key: self.key,
            kind: self.kind,
            revision,
            bytes: self.bytes,
            metadata: self.metadata,
            yjs,
            origin: self.origin,
        }
    }
}

impl Envelope {
    /// True when the hash matches the bytes.
    #[must_use]
    pub fn verify(&self) -> bool {
        content_hash(&self.bytes) == self.content_hash
    }

    /// The backend vocabulary form.
    #[must_use]
    pub fn to_record(&self) -> cvfs::Record {
        cvfs::Record {
            schema_id: self.schema_id.clone(),
            key: self.key.clone(),
            kind: self.kind.as_str().into(),
            revision: self.revision,
            content_hash: self.content_hash.clone(),
            bytes: self.bytes.clone(),
            metadata: Value::Object(self.metadata.clone()).to_string(),
            yjs: self.yjs.clone(),
            origin: self.origin.clone(),
        }
    }

    /// Read a backend record back; `None` when the kind or the metadata is not one this version
    /// wrote.
    #[must_use]
    pub fn from_record(r: cvfs::Record) -> Option<Self> {
        let Value::Object(metadata) = serde_json::from_str(&r.metadata).ok()? else {
            return None;
        };
        Some(Self {
            schema_id: r.schema_id,
            key: r.key,
            kind: Kind::parse(&r.kind)?,
            revision: r.revision,
            content_hash: r.content_hash,
            bytes: r.bytes,
            metadata,
            yjs: r.yjs,
            origin: r.origin,
        })
    }
}
