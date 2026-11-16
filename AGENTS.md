# Agent instructions

`cvws` is the storage workspace: envelope, `Store` trait, backend choice. Keep it that way.

- It never depends on `ccvl`, and never parses document bytes, metadata or Yjs.
- Backends are `cvfs` and `cvdb`, git dependencies on `branch = "main"`; published crates by caret.
- Comments and documentation are English.

Checks (CI runs the same): `cargo fmt --all -- --check`,
`cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked`.
