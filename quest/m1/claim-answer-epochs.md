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
content. The answer has two steps, so the broadcast holds its tracks (or a
`broadcast::Dynamic`) before anyone resolves through it, and the caller names
the epoch. Roughly:

    impl Request {
        /// Start answering the path as a new publisher instance under `epoch`, unannounced until published.
        pub fn prepare(self, epoch: Epoch) -> Result<Prepared, Error>;
    }

    impl Prepared {
        /// Announce the filled broadcast at exactly the requested path and resolve every waiting requester.
        pub fn publish(self) -> Result<broadcast::Producer, Error>;
    }

`Prepared` (name open) gives access to the broadcast to fill and is the only
way to answer, so an announced empty answer can't be expressed. Dropping it
unpublished rejects the request, as dropping a `Request` does. The broadcast
is announced with the claim's route metadata and that epoch, and requesters
resolve through that route, never the claim's instance. Closing it ends the
route; the next request prepares under a new epoch (`Epoch::mint()` per run,
or a replica set's shared one). Session forwarding keeps a crate-private
accept. Rejected: a single `publish` that announces an empty broadcast,
renewing the prefix on each restart (resets every sibling), and carrying
sequences over to the next answer (relabels a different job as a
continuation).

Decided 2026-10-12: public claims carry no epoch. A claim is a capability,
as announce takeover already treats it, so `Producer::dynamic` (JS and the
bindings alike) cannot take one; prefer a shape where the route has none over
a runtime refusal. A request pinned to an epoch resolves only through an
exact-path route with that epoch. Sessions keep epochs on the peer routes
they land internally. Migrate the in-repo callers and tests that claim with
an epoch.

Required: validity and route insertion are atomic. A request the claim
released (an `update` to another instance) or abandoned (the claim dropped)
before `publish` is refused, never announced, as `accept` drops it today.
Regression tests cover both, and a pinned request.

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
and behind an edge relay of ours, on lite-05, 06, 07 and moq-transport 14,
16, 22: the returning reader gets the new output from group 0, never the old
group, and sibling paths keep their readers. On the wires without an epoch
that rests on our relays retiring a copy on End then Start, as
`tests/restart.rs` shows; arbitrary older caching relays are out of scope, as
in [IETF epochs](/quest/m1/ietf-epochs.md), since base IETF object identity
survives a withdrawal. A same-epoch lagging replica still resumes, checked on
lite-07 only, the one wire here that carries the epoch. JS mirrors the tests.
`just test interop --all`.

Public API: breaking. A claim request is answered through `prepare` and
`publish`, and a public claim takes no epoch, in Rust, JS, moq-ffi, moq-c and
every binding wrapper, plus the claim docs. Wire: none.

## Related

- [Announcement takeover](/quest/m1/announce-takeover.md) - its claim-to-route operation is `Request::prepare` then `publish`; TAKEOVER then removes the cold-start reset
- [IETF epochs](/quest/m1/ietf-epochs.md) - carries the answer epoch over moq-transport, so same-epoch resume and replacement can be told apart there
- [#5057](https://github.com/moq-dev/moq/pull/5057) - the relay-side check this replaces
