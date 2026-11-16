//! `cvws`: the `CareerVector` storage workspace.
//!
//! It selects a storage backend ([`Backend`]: `cvfs` files or a `cvdb` SQLite database), owns the
//! document [`Envelope`] and the [`Store`] trait, and guarantees conditional writes (compare-and-swap
//! on revision and Yjs clock), revision and hash integrity, receipts and retained snapshots.
//!
//! It never interprets a document and never merges: Yjs/Yrs bytes, clocks and op logs are preserved
//! as they came. Applying ops to a document is the engine's job (`ccvl::session`), which persists
//! through this crate. [`convert`] moves a whole workspace between backends without loss.

#![forbid(unsafe_code)]

pub mod envelope;
pub mod store;

pub use cvfs::{Commit, Error as StoreError};
pub use envelope::{Draft, Envelope, Kind, Op, Origin, Receipt, Yjs, content_hash};
pub use store::{Backend, RemoteStore, Store, Update, convert};
