# [L] A claim answers each output as its own epoch

## Goal

A worker behind a prefix claim that restarts a path's output does so under a
new epoch, so every hop sees the change and drops its copy: a returning reader
never gets the old output's groups. The public claim API cannot serve two
outputs at one path under the same instance. Replaces the relay-side
regression check proposed in #5057.

## Plan

Facts as of main 90500867d:

- A claim (`origin::Producer::dynamic`, JS `Producer.dynamic`, moq-ffi, moq-c
  and the binding wrappers) answers with any broadcast via `Request::accept`.
  Every answer resolves under the claim route's instance (its epoch, or the
  entry's generation), shared by every path and every answer for the claim's
  lifetime. Once an answer closes, the handler is asked again and the new
  broadcast, restarting at group 0, resolves under the same instance; nothing
  on any wire changes, so a lingering downstream copy splices old onto new.
- `Dynamic::update` to a new epoch, or a renewal, restarts the whole prefix,
  resetting every sibling path's readers. Too coarse for a pool.
- Tracks already avoid this: a broadcast keeps each track name's sequence
  across replacement producers. Claims have no equivalent.
- In repo, the production claim handlers are moq-net sessions forwarding a
  peer's claim; they relay upstream numbering and keep working as is.

Decided 2026-10-12: a request is answered by publishing, not by handing over
content, and the caller names the epoch:

    impl Request {
        /// Serve the path as a new publisher instance, announced at exactly this path under `epoch`.
        pub fn publish(self, epoch: Epoch) -> Result<broadcast::Producer, Error>;
    }

The broadcast is announced at the requested path with the claim's route
metadata and that epoch, and requesters resolve through that route, never the
claim's instance. Closing it ends the route; the next request publishes
under a new epoch (`Epoch::mint()` per run, or a replica set's shared one).
Session forwarding keeps a crate-private accept. Rejected: renewing the
prefix on each restart (resets every sibling), and carrying sequences over to
the next answer (relabels a different job as a continuation).

Open, for the maintainer: a requester that resolves through an announced but
still empty broadcast gets `NotFound` for its first track, so the handler
must create its tracks (or a `broadcast::Dynamic`) before anyone resolves.
`publish` as sketched announces too early. Options, keeping the name and the
explicit epoch:

- A typed two-phase answer: `request.prepare(epoch)` returns an unannounced
  handle to fill, and its `publish()` announces and resolves (recommended:
  misuse can't be expressed).
- `request.publish(broadcast, epoch)` taking a broadcast from
  `create_broadcast(request.path())`, refusing a wrong path or an announced
  one at runtime.

Required: validity and route insertion are atomic. A request the claim
released (an `update` to another instance) or abandoned (the claim dropped)
before the handler publishes is refused, never announced, as `accept` drops
it today. Regression tests cover both.

Look out for:

- A downstream relay that resolved the path through its copy of the claim
  before the exact announcement arrived holds a front under the claim's
  instance. Check it can't be rejoined once another route won the path and
  then ended; if it can, retire it per path, not by renewing the prefix. The
  cold-start reset it implies is accepted until announce takeover lands.
- One exact announcement per live output: benchmark outputs against downstream
  cursors.
- No producer-side refusal of a backwards sequence: a fresh broadcast has no
  memory of the last run, and backfill below the newest group is legitimate.

Verification: mocked time, reusing #5057's worker harness
(`tests/largest_regression.rs` on its branch, with Dryvnt credited as
co-author). A worker restarts `pool/p` within the linger, behind one relay
and behind an edge relay, on lite-05, 06, 07 and moq-transport 14, 16, 22:
the returning reader gets the new output from group 0, never the old group,
and sibling paths keep their readers. A same-epoch lagging replica still
resumes. JS mirrors the tests. `just test interop --all`.

Public API: breaking change to how a claim request is answered, in Rust, JS,
moq-ffi, moq-c and every binding wrapper, plus the claim docs. Wire: none.

## Related

- [Announcement takeover](/quest/m1/announce-takeover.md) - its claim-to-route operation is `Request::publish`; TAKEOVER then removes the cold-start reset
- [#5057](https://github.com/moq-dev/moq/pull/5057) - the relay-side check this replaces
