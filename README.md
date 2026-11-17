# cvws

The CareerVector storage workspace. It selects the storage backend (`cvfs` files or `cvdb` SQLite),
owns the document envelope and the `Store` trait, and guarantees conditional writes (compare-and-swap
on revision and Yjs clock), revision and hash integrity, receipts and retained snapshots.

It never interprets or merges documents; Yjs/Yrs bytes, clocks and op logs are preserved. The session
API that applies ops to a document lives in the engine (`ccvl::session`) and persists through `cvws`.
`convert` moves a whole workspace between backends without loss. `export_bundle` / `import_bundle` write and read a whole workspace (documents with their
Yjs state, retained revisions, op logs, receipts, assets) as one sealed, byte-exact JSON bundle; a
remote handle bundles the sub-documents named with `RemoteStore::track`.

Licence: FSL-1.1-ALv2.
