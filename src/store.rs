//! The `Store` trait and the backend choice.
//!
//! A store keeps envelopes per workspace and guarantees conditional writes (compare-and-swap on
//! revision and, for Yjs sub-documents, on the clock), revision and hash integrity, receipts and
//! retained snapshots. It never merges and never interprets a payload.

use std::path::Path;
use std::sync::{Arc, Mutex};

use cvdb::http::Http;
use cvdb::remote::{Outcome, Remote};
use cvdb::store::SqliteStore;
use cvfs::{Commit, Dump, Error, FsStore, Result};

use crate::envelope::{Draft, Envelope, Kind, Op, Origin, Receipt, Yjs, content_hash};

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
    /// A light server reached over HTTP (`cvdb::remote`): one workspace, sub-documents as keys.
    Remote(RemoteStore),
}

impl Backend {
    pub fn open_fs(root: impl AsRef<Path>) -> Result<Self> {
        FsStore::open(root.as_ref()).map(Self::Fs)
    }

    pub fn open_sqlite(path: impl AsRef<Path>) -> Result<Self> {
        SqliteStore::open(path).map(Self::Sqlite)
    }

    /// A light-server workspace; `dir` holds the durable outbox.
    pub fn open_remote(
        http: Arc<dyn Http>,
        api_base: &str,
        ws_id: &str,
        dir: impl AsRef<Path>,
    ) -> Result<Self> {
        RemoteStore::open(http, api_base, ws_id, dir.as_ref()).map(Self::Remote)
    }

    fn store(&self) -> &dyn Store {
        match self {
            Self::Fs(s) => s,
            Self::Sqlite(s) => s,
            Self::Remote(s) => s,
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

fn unsupported<T>(what: &str) -> Result<T> {
    Err(Error::Invalid(format!(
        "{what} is not supported by the remote backend"
    )))
}

/// The light-server backend: one workspace, its sub-documents (`jobs`, `settings`, ...) as keys.
/// It is a byte transport, so it speaks the Yjs subset of [`Store`]: `commit` appends an update
/// (the record's metadata carries the typed op envelope as `{"op": {...}, "opId": "..."}`, opaque
/// here), `get_record` reads the head, `ops_since` reads the tail, `receipts` are the ones this
/// handle received. A commit made while the server is unreachable waits in the durable outbox and
/// reports `WouldBlock`; [`RemoteStore::flush`] replays it. Listing, retained snapshots, assets,
/// `dump` and `restore` do not exist on the server and are refused.
pub struct RemoteStore {
    remote: Remote,
    ws: String,
    receipts: Mutex<Vec<Receipt>>,
    keys: Mutex<Vec<String>>,
}

impl RemoteStore {
    pub fn open(http: Arc<dyn Http>, api_base: &str, ws_id: &str, dir: &Path) -> Result<Self> {
        let remote = Remote::new(http, api_base, ws_id, dir)
            .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
        Ok(Self {
            remote,
            ws: ws_id.into(),
            receipts: Mutex::new(Vec::new()),
            keys: Mutex::new(Vec::new()),
        })
    }

    /// Name the sub-documents this handle bundles. The server cannot list them, so `keys`, `dump`
    /// and `restore` (and with them the workspace bundle) work on exactly these.
    pub fn track(&self, keys: &[&str]) {
        if let Ok(mut k) = self.keys.lock() {
            *k = keys.iter().map(|s| (*s).to_owned()).collect();
            k.sort();
            k.dedup();
        }
    }

    fn tracked(&self) -> Result<Vec<String>> {
        let k = self
            .keys
            .lock()
            .map_err(|_| Error::Integrity("keys lock poisoned".into()))?;
        if k.is_empty() {
            return unsupported("listing keys (call track first)");
        }
        Ok(k.clone())
    }

    /// Replay the outbox; see [`Remote::flush`].
    pub fn flush(&self) -> Result<cvdb::remote::Flushed> {
        self.remote
            .flush()
            .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))
    }

    /// Entries still waiting for the server.
    pub fn queued(&self) -> usize {
        self.remote.queued()
    }

    fn own(&self, ws: &str) -> Result<()> {
        if ws == self.ws {
            Ok(())
        } else {
            Err(Error::Invalid(format!(
                "this remote handle serves workspace {}",
                self.ws
            )))
        }
    }

    fn io(e: &cvdb::Error) -> Error {
        Error::Io(std::io::Error::other(e.to_string()))
    }
}

impl Store for RemoteStore {
    fn commit(&self, ws: &str, c: &Commit) -> Result<Receipt> {
        self.own(ws)?;
        let rec = &c.record;
        let (Some(y), Some(op)) = (&rec.yjs, &c.op) else {
            return Err(Error::Invalid(
                "the remote backend stores Yjs updates only".into(),
            ));
        };
        let meta: serde_json::Value = serde_json::from_str(&rec.metadata)
            .map_err(|_| Error::Invalid("metadata is not JSON".into()))?;
        let envelope = meta
            .get("op")
            .cloned()
            .ok_or_else(|| Error::Invalid("metadata needs the op envelope under \"op\"".into()))?;
        let op_id = meta
            .get("opId")
            .and_then(|v| v.as_str())
            .map_or_else(|| content_hash(&op.bytes), str::to_owned);
        let updates = std::collections::BTreeMap::from([(rec.key.clone(), op.bytes.clone())]);
        let expected = std::collections::BTreeMap::from([(rec.key.clone(), y.clock - 1)]);
        match self.remote.append(&op_id, &envelope, &updates, &expected) {
            Ok(Outcome::Done(done)) => {
                let clock = done
                    .iter()
                    .find(|d| d.sub_doc == rec.key)
                    .map_or(y.clock, |d| d.clock);
                let mut list = self
                    .receipts
                    .lock()
                    .map_err(|_| Error::Integrity("receipts lock poisoned".into()))?;
                let receipt = Receipt {
                    seq: list.len() as u64 + 1,
                    key: rec.key.clone(),
                    revision: clock,
                    clock: Some(clock),
                    content_hash: content_hash(&op.bytes),
                    at_ms: c.at_ms,
                };
                list.push(receipt.clone());
                Ok(receipt)
            }
            Ok(Outcome::Queued) => Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "queued in the outbox",
            ))),
            // The server's clock is ahead of the one the commit builds on (how far it is unknown).
            Err(cvdb::Error::Conflict) => Err(Error::Conflict {
                key: rec.key.clone(),
                expected: y.clock - 1,
                actual: y.clock,
            }),
            Err(e) => Err(Self::io(&e)),
        }
    }

    fn get_record(&self, ws: &str, key: &str) -> Result<Option<cvfs::Record>> {
        self.own(ws)?;
        let f = self
            .remote
            .fetch(key, None)
            .map_err(|e| Self::io(&e))?
            .ok_or_else(|| Error::Integrity("unexpected 304".into()))?;
        if f.clock == 0 && f.head.is_empty() {
            return Ok(None);
        }
        Ok(Some(cvfs::Record {
            schema_id: "careervector.yjs-subdoc/v1".into(),
            key: key.into(),
            kind: Kind::CrdtSnapshot.as_str().into(),
            revision: f.clock,
            content_hash: content_hash(&f.head),
            bytes: f.head.clone(),
            metadata: format!("{{\"headClock\":{}}}", f.head_clock()),
            yjs: Some(Yjs {
                state_vector: Vec::new(),
                clock: f.clock,
            }),
            origin: None,
        }))
    }

    fn keys(&self, ws: &str) -> Result<Vec<String>> {
        self.own(ws)?;
        self.tracked()
    }

    fn ops_since(&self, ws: &str, key: &str, after: u64) -> Result<Vec<Op>> {
        self.own(ws)?;
        let f = self
            .remote
            .fetch(key, None)
            .map_err(|e| Self::io(&e))?
            .ok_or_else(|| Error::Integrity("unexpected 304".into()))?;
        if after < f.head_clock() {
            return Err(Error::Invalid(
                "those updates were folded into the head; reload the head".into(),
            ));
        }
        let base = f.head_clock();
        Ok(f.tail
            .into_iter()
            .enumerate()
            .map(|(i, bytes)| Op {
                clock: base + i as u64 + 1,
                actor_class: "unknown".into(),
                actor_id: String::new(),
                at_ms: 0,
                bytes,
            })
            .filter(|o| o.clock > after)
            .collect())
    }

    fn receipts(&self, ws: &str, after: u64) -> Result<Vec<Receipt>> {
        self.own(ws)?;
        let list = self
            .receipts
            .lock()
            .map_err(|_| Error::Integrity("receipts lock poisoned".into()))?;
        Ok(list.iter().filter(|r| r.seq > after).cloned().collect())
    }

    fn retained_records(&self, _: &str, _: &str) -> Result<Vec<cvfs::Record>> {
        unsupported("retained snapshots")
    }

    fn put_asset(&self, _: &str, _: &str, _: &[u8]) -> Result<()> {
        unsupported("assets")
    }

    fn get_asset(&self, _: &str, _: &str) -> Result<Option<Vec<u8>>> {
        unsupported("assets")
    }

    /// The tracked sub-documents: the heads, the tails (clocks after the head) and this handle's
    /// receipts. No retained snapshots and no assets exist on the server.
    fn dump(&self, ws: &str) -> Result<Dump> {
        self.own(ws)?;
        let mut d = Dump::default();
        for key in self.tracked()? {
            let Some(rec) = self.get_record(ws, &key)? else {
                continue;
            };
            let head = self
                .remote
                .fetch(&key, None)
                .map_err(|e| Self::io(&e))?
                .ok_or_else(|| Error::Integrity("unexpected 304".into()))?
                .head_clock();
            for op in self.ops_since(ws, &key, head)? {
                d.ops.push((key.clone(), op));
            }
            d.records.push(rec);
        }
        d.receipts = self.receipts(ws, 0)?;
        Ok(d)
    }

    /// Replay the updates of an empty server workspace in clock order. A folded head cannot be
    /// rebuilt through the transport and is refused. Afterwards this handle's receipts are the
    /// bundle's.
    fn restore(&self, ws: &str, d: &Dump) -> Result<()> {
        self.own(ws)?;
        if !d.retained.is_empty() || !d.assets.is_empty() {
            return unsupported("retained snapshots and assets");
        }
        let tracked = self.tracked()?;
        for rec in &d.records {
            if !tracked.contains(&rec.key) {
                return Err(Error::Invalid(format!(
                    "{}: not a tracked sub-document",
                    rec.key
                )));
            }
            if self.get_record(ws, &rec.key)?.is_some() {
                return Err(Error::Invalid(format!(
                    "{}: the server workspace is not empty",
                    rec.key
                )));
            }
            if !rec.bytes.is_empty() {
                return Err(Error::Invalid(format!(
                    "{}: a folded head cannot be restored through the transport",
                    rec.key
                )));
            }
        }
        let mut ops: Vec<&(String, Op)> = d.ops.iter().collect();
        ops.sort_by(|a, b| (&a.0, a.1.clock).cmp(&(&b.0, b.1.clock)));
        for (key, op) in ops {
            let metadata = serde_json::json!({
                "op": {"kind": "bundle-import"},
                "opId": format!("{key}:{}:{}", op.clock, content_hash(&op.bytes)),
            });
            let commit = Commit {
                record: cvfs::Record {
                    schema_id: "careervector.yjs-subdoc/v1".into(),
                    key: key.clone(),
                    kind: Kind::CrdtUpdate.as_str().into(),
                    revision: op.clock,
                    content_hash: content_hash(&op.bytes),
                    bytes: op.bytes.clone(),
                    metadata: metadata.to_string(),
                    yjs: Some(Yjs {
                        state_vector: Vec::new(),
                        clock: op.clock,
                    }),
                    origin: None,
                },
                op: Some(op.clone()),
                at_ms: op.at_ms,
            };
            self.commit(ws, &commit)?;
        }
        self.receipts
            .lock()
            .map_err(|_| Error::Integrity("receipts lock poisoned".into()))?
            .clone_from(&d.receipts);
        Ok(())
    }
}
