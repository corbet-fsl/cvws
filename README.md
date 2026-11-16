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
| `Workspace::take_outgoing()` | the bytes local edits produced, per sub-document (a `Result`: bytes are never lost) |
| `Workspace::view(sub, branch, node_path)` | read one node |

Two more calls serve hosts that talk to a server which applies ops itself:

* `Workspace::prepare(op, actor)` builds and validates an op envelope against
  copies of the documents without applying it. Send the envelope; the server's
  canonical bytes come back through `receive`, so the edit is not applied twice.
* `Workspace::type_text(field, range, text, actor)` is the keystroke path for prose. A refused call
  changes nothing and emits nothing; a non-human actor cannot edit a `FIXED` node.
* `prepare` also writes the ids the engine would generate (`node.create` ids, `cvl.*` variant ids)
  into the envelope, so the server produces the same ones.

Every mutating call returns the nodes that changed, by location (`NodeRef`: the branch
`<profile>.<lang>` and the dot-joined id path), because ids repeat across language trees.
A caller reads only those.

`receive` without a `hint` routes each frame by structure, never by what it would change, so no
update is lost (relay frames carry no sub-document tag): a frame naming `cv_profile` or `cl_profile`
goes there; a frame naming only other roots (`jobs`, `settings`, ...) is dropped on purpose and counted
(`Applied::foreign`); a frame of a client a sub-document already knows goes to that sub-document (on the
web every sub-document has its own client id); a frame from an unseen client goes to the one sub-document
it integrates into without waiting for history; anything else is held (`Applied::unrouted`) and retried
after every `receive`. A frame whose history is still missing waits, pending, in its sub-document.

Op shapes are the op catalogue of CareerVector (`OP_CATALOG_VERSION`, re-exported
from `ccvl-core`). Ops for other sub-documents (`job.*`, `settings.update`, ...) are
refused with a `cvws:` error.

```sh
cargo test --locked
```

## Licence

FSL-1.1-ALv2, see `LICENSE.md`.
