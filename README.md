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
| `Workspace::take_outgoing()` | the bytes local edits produced, per sub-document (a `Result`; `restore_outgoing` gives them back) |
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

`receive` without a `hint` (relay frames carry no sub-document tag) attributes each frame by structure,
never by what it would change on screen, and never by forking a document:

* the roots a frame names and the clients it depends on (parents, origins, deleted items, its authors'
  earlier history) are read once, by applying the frame to an empty throw-away document;
* a dependency on a client that a sub-document already holds belongs to that sub-document (on the web every
  sub-document has its own client id); one on a client seen only in foreign frames is foreign;
* a frame of one sub-document is applied there; if its history is missing, `yrs` keeps it pending;
* a purely foreign frame (`jobs`, `settings`, ...) is dropped on purpose and counted (`Applied::foreign`); its
  clients are remembered, so the nested keystroke frames that follow are recognised as foreign too;
* a frame that mixes sub-documents cannot be split: it is not applied, counted (`Applied::mixed`) and the resync
  flag is raised, so a document is never polluted;
* a frame whose sub-document is not known yet is held in a bounded buffer (`MAX_HELD`, 256), retried after every
  `receive` (hinted ones too), and pushed out oldest first when full: counted (`Applied::dropped`), flag raised;
  a held frame that fails is dropped alone, the ones behind it are still tried.

`Workspace::take_resync()` returns true once when frames were lost, mixed or refused since the last call; the host
resyncs the affected sub-documents and rate-limits doing so (every REST read costs a Worker request).
`Applied::failed` carries per-sub-document failures next to what applied elsewhere.

## Hostile frames

`yrs` decodes plain nested values recursively (`Any::decode`), so the size of a frame bounds the stack a hostile
one can use. `receive` refuses frames over `MAX_FRAME_BYTES` (1 MiB) before decoding them. A nested array costs
two bytes per level, so 1 MiB can carry about 524,000 levels. Measured in a release build (decode and drop of one
frame in a thread of the given stack; each cell is a separate process):

| thread stack | 5,000 levels | 20,000 | 100,000 | 524,000 |
|---|---|---|---|---|
| 2 MiB | ok | overflow | overflow | overflow |
| 8 MiB | ok | ok | overflow | overflow |
| 64 MiB | ok | ok | ok | overflow |
| 256 MiB | ok | ok | ok | ok |

So the worst frame under the cap needs a stack of 256 MiB (about 500 bytes per level; debug builds need more).
Hosts run every call that touches received bytes (`receive`, `encode_state`, `take_outgoing`) on a thread with that
stack; it is reserved address space and only touched by a hostile frame. A host that cannot spend that lowers the
frame size it passes in: 64 MiB is enough for frames up to about 200 KB. What the engine itself walks is iterative
or depth-capped (trees 128 levels, JSON 256), so past decoding nothing recurses without a limit.

Op shapes are the op catalogue of CareerVector (`OP_CATALOG_VERSION`, re-exported
from `ccvl-core`). Ops for other sub-documents (`job.*`, `settings.update`, ...) are
refused with a `cvws:` error.

```sh
cargo test --locked
```

## Licence

FSL-1.1-ALv2, see `LICENSE.md`.
