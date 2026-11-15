# cvws

<!-- ccid:security:begin (rendered by ccid render; do not edit by hand) -->
**Security and dependencies:** watched by Renovate (with OSV vulnerability alerts); an update is merged only when its tests pass. See the [security policy](SECURITY.md).
<!-- ccid:security:end -->

The workspace API over the [`ccvl`](https://git.corbet.ch/corbet-fsl/ccvl) document engine.

A CareerVector workspace is partitioned into sub-documents. The two profile
sub-documents, `cv-profile` and `cl-profile`, hold the CV and cover-letter trees
that `ccvl-core` (feature `doc`) edits as a Yrs document. `cvws` keeps one engine
document per profile sub-document and gives a host (a terminal UI, a CLI, a sync
backend) one small surface. It does no I/O and never interprets what the
documents say.

| call | what it does |
|---|---|
| `Workspace::open(states)` | load sub-documents from stored Yjs state |
| `Workspace::apply(op, actor)` | route an op or a batch to its sub-documents and apply it |
| `Workspace::receive(update, hint)` | take update bytes from the network or a store |
| `Workspace::take_outgoing()` | the bytes local edits produced, per sub-document |
| `Workspace::view(sub, branch, node_path)` | read one node |

Two more calls serve hosts that talk to a server which applies ops itself:

* `Workspace::prepare(op, actor)` builds and validates an op envelope against
  copies of the documents without applying it. Send the envelope; the server's
  canonical bytes come back through `receive`, so the edit is not applied twice.
* `Workspace::type_text(...)` is the keystroke path for prose.

Every mutating call returns the ids that changed. A caller reads only those.

`receive` without a `hint` offers the update to every sub-document, because relay
frames carry no sub-document tag. Each sub-document tries the update on a copy and
keeps it only if its own profile root changes, so an update meant for another
sub-document leaves no foreign roots and no pending state behind.

Op shapes are the op catalogue of CareerVector (`OP_CATALOG_VERSION`, re-exported
from `ccvl-core`). Ops for other sub-documents (`job.*`, `settings.update`, ...) are
refused with a `cvws:` error.

```sh
cargo test --locked
```

## Licence

FSL-1.1-ALv2, see `LICENSE.md`.
