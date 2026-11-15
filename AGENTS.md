# Agent instructions

`cvws` is a small public library: the workspace API over the `ccvl-core` document
engine (`ccvl` repository, feature `doc`). Keep it that way.

- No I/O and no knowledge of CV content. Storage, sync and rendering belong to
  the hosts and to sibling libraries (`cvfs`, `cvdb`).
- The engine owns the layout and the op semantics. `cvws` only routes ops to
  sub-documents, groups batches, and offers incoming updates; do not copy engine
  logic here.
- Libraries stay separate repositories. Depend on `ccvl-core` as a cargo git
  dependency, never as a path or a copy.
- Comments and documentation are English.

Checks (CI runs the same): `cargo fmt --all -- --check`,
`cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked`.
