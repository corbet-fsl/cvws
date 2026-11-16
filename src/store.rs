//! The `Store` trait and the backend choice.
//!
//! A store keeps envelopes per workspace and guarantees conditional writes (compare-and-swap on
//! revision and, for Yjs sub-documents, on the clock), revision and hash integrity, receipts and
//! retained snapshots. It never merges and never interprets a payload.

use std::path::Path;

use cvdb::store::SqliteStore;
use cvfs::{Commit, Dump, Error, FsStore, Result};

use crate::envelope::{Draft, Envelope, Kind, Op, Origin, Receipt, Yjs};

/// One backend of the storage workspace.
pub trait Store {
    /// Apply a conditional write. See [`Commit`].
    fn commit(&self, ws: &str, c: &Commit) -> Result<Receipt>;
    fn get_record(&self, ws: &str, key: &str) -> Result<Option<cvfs::Record>>;
    fn keys(&self, ws: &str) -> Result<Vec<String>>;
    fn ops_since(&self, ws: &str, key: &str, after: u64) -> Result<Vec<Op>>;
    fn receipts(&self, ws: &str, after: u64) -> Result<Vec<Receipt>>;
    fn retained_records(&self, ws: &str, key: &str) -> Result<Vec<cvfs::Record>>;
    fn put_asset(&self, ws: &str, path: &str, bytes: &[u8]) -> Result<()>;
    fn get_asset(&self, ws: &str, path: &str) -> Result<Option<Vec<u8>>>;
    fn dump(&self, ws: &str) -> Result<Dump>;
    fn restore(&self, ws: &str, d: &Dump) -> Result<()>;

    /// The current envelope of a document.
    fn get(&self, ws: &str, key: &str) -> Result<Option<Envelope>> {
        self.get_record(ws, key)?
            .map(|r| {
                Envelope::from_record(r)
                    .ok_or_else(|| Error::Integrity(format!("{key}: not an envelope")))
            })
            .transpose()
    }

    /// Earlier revisions of a document, oldest first.
    fn retained(&self, ws: &str, key: &str) -> Result<Vec<Envelope>> {
        self.retained_records(ws, key)?
            .into_iter()
            .map(|r| {
                Envelope::from_record(r)
                    .ok_or_else(|| Error::Integrity(format!("{key}: not an envelope")))
            })
            .collect()
    }

    /// Write a projected or authored document on top of `expected_revision` (0 for a new one).
    /// A different stored revision is refused with [`Error::Conflict`] and nothing changes.
    fn put(&self, ws: &str, draft: Draft, expected_revision: u64, at_ms: u64) -> Result<Receipt> {
        let mut draft = draft;
        if let Some(o) = draft.origin.as_mut() {
            o.parent_revision = expected_revision;
        }
        let record = draft.at(expected_revision + 1, None).to_record();
        self.commit(
            ws,
            &Commit {
                record,
                op: None,
                at_ms,
            },
        )
    }

    /// Commit one Yjs update to a sub-document: the new compressed snapshot, its state vector and
    /// the update bytes, on top of `expected_clock` (0 for a new sub-document). A different stored
    /// clock is refused with [`Error::Conflict`]; the caller then rebases and retries.
    fn append_update(&self, ws: &str, update: &Update<'_>) -> Result<Receipt> {
        let current = self.get_record(ws, update.key)?;
        let revision = current.as_ref().map_or(0, |r| r.revision);
        let clock = current
            .as_ref()
            .and_then(|r| r.yjs.as_ref())
            .map_or(0, |y| y.clock);
        if clock != update.expected_clock {
            return Err(Error::Conflict {
                key: update.key.into(),
                expected: update.expected_clock,
                actual: clock,
            });
        }
        let new_clock = clock + 1;
        let draft = Draft {
            schema_id: update.schema_id.into(),
            key: update.key.into(),
            kind: Kind::CrdtSnapshot,
            bytes: update.snapshot.to_vec(),
            metadata: update.metadata.clone(),
            origin: Some(Origin {
                actor: update.actor_id.into(),
                device: update.device.into(),
                parent_revision: revision,
            }),
        };
        let yjs = Yjs {
            state_vector: update.state_vector.to_vec(),
            clock: new_clock,
        };
        let record = draft.at(revision + 1, Some(yjs)).to_record();
        let op = Op {
            clock: new_clock,
            actor_class: update.actor_class.into(),
            actor_id: update.actor_id.into(),
            at_ms: update.at_ms,
            bytes: update.bytes.to_vec(),
        };
        self.commit(
            ws,
            &Commit {
                record,
                op: Some(op),
                at_ms: update.at_ms,
            },
        )
    }
}

/// One Yjs update to commit with [`Store::append_update`].
#[derive(Clone, Debug)]
pub struct Update<'a> {
    pub schema_id: &'a str,
    pub key: &'a str,
    /// The clock the client's snapshot was read at; 0 when the sub-document is new.
    pub expected_clock: u64,
    /// The compressed snapshot after the update.
    pub snapshot: &'a [u8],
    pub state_vector: &'a [u8],
    /// The update bytes, as broadcast.
    pub bytes: &'a [u8],
    pub actor_class: &'a str,
    pub actor_id: &'a str,
    pub device: &'a str,
    pub at_ms: u64,
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

macro_rules! delegate {
    ($ty:ty) => {
        impl Store for $ty {
            fn commit(&self, ws: &str, c: &Commit) -> Result<Receipt> {
                <$ty>::commit(self, ws, c)
            }
            fn get_record(&self, ws: &str, key: &str) -> Result<Option<cvfs::Record>> {
                <$ty>::get(self, ws, key)
            }
            fn keys(&self, ws: &str) -> Result<Vec<String>> {
                <$ty>::keys(self, ws)
            }
            fn ops_since(&self, ws: &str, key: &str, after: u64) -> Result<Vec<Op>> {
                <$ty>::ops_since(self, ws, key, after)
            }
            fn receipts(&self, ws: &str, after: u64) -> Result<Vec<Receipt>> {
                <$ty>::receipts(self, ws, after)
            }
            fn retained_records(&self, ws: &str, key: &str) -> Result<Vec<cvfs::Record>> {
                <$ty>::retained(self, ws, key)
            }
            fn put_asset(&self, ws: &str, path: &str, bytes: &[u8]) -> Result<()> {
                <$ty>::put_asset(self, ws, path, bytes)
            }
            fn get_asset(&self, ws: &str, path: &str) -> Result<Option<Vec<u8>>> {
                <$ty>::get_asset(self, ws, path)
            }
            fn dump(&self, ws: &str) -> Result<Dump> {
                <$ty>::dump(self, ws)
            }
            fn restore(&self, ws: &str, d: &Dump) -> Result<()> {
                <$ty>::restore(self, ws, d)
            }
        }
    };
}

delegate!(FsStore);
delegate!(SqliteStore);

/// The storage backend a workspace runs on, chosen at open time.
pub enum Backend {
    /// Local/offline workspace on a directory (`cvfs`).
    Fs(FsStore),
    /// Database-backed workspace (`cvdb`, `SQLite`).
    Sqlite(SqliteStore),
}

impl Backend {
    pub fn open_fs(root: impl AsRef<Path>) -> Result<Self> {
        FsStore::open(root.as_ref()).map(Self::Fs)
    }

    pub fn open_sqlite(path: impl AsRef<Path>) -> Result<Self> {
        SqliteStore::open(path).map(Self::Sqlite)
    }

    fn store(&self) -> &dyn Store {
        match self {
            Self::Fs(s) => s,
            Self::Sqlite(s) => s,
        }
    }
}

impl Store for Backend {
    fn commit(&self, ws: &str, c: &Commit) -> Result<Receipt> {
        self.store().commit(ws, c)
    }
    fn get_record(&self, ws: &str, key: &str) -> Result<Option<cvfs::Record>> {
        self.store().get_record(ws, key)
    }
    fn keys(&self, ws: &str) -> Result<Vec<String>> {
        self.store().keys(ws)
    }
    fn ops_since(&self, ws: &str, key: &str, after: u64) -> Result<Vec<Op>> {
        self.store().ops_since(ws, key, after)
    }
    fn receipts(&self, ws: &str, after: u64) -> Result<Vec<Receipt>> {
        self.store().receipts(ws, after)
    }
    fn retained_records(&self, ws: &str, key: &str) -> Result<Vec<cvfs::Record>> {
        self.store().retained_records(ws, key)
    }
    fn put_asset(&self, ws: &str, path: &str, bytes: &[u8]) -> Result<()> {
        self.store().put_asset(ws, path, bytes)
    }
    fn get_asset(&self, ws: &str, path: &str) -> Result<Option<Vec<u8>>> {
        self.store().get_asset(ws, path)
    }
    fn dump(&self, ws: &str) -> Result<Dump> {
        self.store().dump(ws)
    }
    fn restore(&self, ws: &str, d: &Dump) -> Result<()> {
        self.store().restore(ws, d)
    }
}

/// Move a whole workspace from one backend to another without loss: documents, revisions, hashes,
/// Yjs clocks and op logs, receipts, retained snapshots and assets. The destination workspace must
/// be empty. Afterwards the destination is read back and compared with the source; a difference is
/// an error.
pub fn convert(src: &dyn Store, dst: &dyn Store, ws: &str) -> Result<Dump> {
    let dump = src.dump(ws)?;
    dst.restore(ws, &dump)?;
    if dst.dump(ws)? != dump {
        return Err(Error::Integrity(format!(
            "workspace {ws}: the copy differs from the source"
        )));
    }
    Ok(dump)
}
