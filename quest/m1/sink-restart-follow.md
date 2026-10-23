# [L] moq-cli and moq-gst readers follow a broadcast's restarts

## Goal

Every moq-cli and moq-gst reader that resolves a broadcast and keeps reading
it follows publisher instances through moq-net's
`origin::Consumer::announced_broadcast`
([#5925](https://github.com/moq-dev/moq/pull/5925)) instead of a hand-rolled
`origin.follow` loop. Each handles an epochless
`Restart` on purpose: it follows the new instance where its output can mark
the break. A route change alone is never fatal: a reader that cannot follow
stays on the old instance while its publisher serves it, and fails once that
route dies, since its subscriptions then end with an error.

## Plan

Decided with the maintainer on 2026-10-20: widened from the server and push
sinks to every moq-cli and moq-gst reader. Following a catalog's references
to other broadcasts is [moqsrc sibling follow](/quest/m0/broadcast-epoch/moqsrc-sibling-follow.md)
for `moqsrc`, through `moq_mux::Source::follow`;
`moq play` resolves a reference once with `Source::resolve`, which waits for
a sibling's first announcement and keeps it.

[Draft PR6526](https://github.com/moq-dev/moq/pull/6526) checkpoints the settled
reader work: `moq play` and `moqsrc` use `moq_mux::follow::Instances` and build
`Source` from the returned broadcast, without requesting the path again.
Playback resets presentation per instance; GStreamer keeps its pads and cancels
the superseded run. A selected handler's explicit refusal is terminal. The
player's refusal test fails on the base implementation, which silently waits.
Epoch, epochless, same-instance route updates and route handoff tests preserve
the existing behavior. A GStreamer test replaces an unanswered broadcast
request and confirms a late stale answer cannot take the replacement's pads.

`transcode` uses the same shared operation to retire the ladder on replacement
and announce a fresh derivative instance with its own epoch, including an
epochless source restart. A clean source end waits for another instance. Its
pending-catalog tests exercise replacement before any encoder is required.
Cancellation tests also reproduced unfinished catalog/rung drops when retiring a
ladder. Private cleanup now belongs to the actual transcoder, rung, and group
writers, including constructed run and rung futures abandoned before their
first poll. Cancellation aborts incomplete output; it does not mark it clean.
The flags and public Rust signatures are unchanged; CLI behavior migration is in
`rs/moq-cli/MIGRATE.md`.

Remaining work and decisions:

- Keep finite `fetch`, non-TS stdout exports, and TS without `--stitch` pinned
  to the instance they resolved. Never concatenate independent instances where
  the output cannot mark a break. These readers are unchanged in the checkpoint.
- TS listen and push SRT both use the existing TS `Follower`, and its stitch
  tests already cover a live overlapping replacement. Moving that follower to
  `Instances` still needs the shared operation to expose unresolved changes:
  `Follower::with_linger` bounds announcement/restart through broadcast
  resolution and the first catalog, but `Instances::Start` is yielded only
  after resolution. Starting the timer there silently extends the public bound.
  The follower also distinguishes a refused return while lingering from a
  refused live switch. Recommend retaining these policies and designing the
  smallest shared unresolved-change operation, rather than shifting the timer,
  adding another reader-local follow loop, or changing refusal behavior.
- HLS has a reproduced overlapping-replacement defect: while the old publisher
  remains live, `Server::broadcaster` returns its cached `Arc<Broadcaster>` before
  resolving the new winner. A paused-time test that publishes two epochs at one
  path fails its assertion that a new request reads a different broadcaster.
  Merely swapping cached broadcasters resets source-derived
  `EXT-X-MEDIA-SEQUENCE` on the stable playlist URI, and does not establish
  discontinuity continuity or safe segment identity. No cache/playlist policy
  change is included. Settle a continuous playlist with monotonic media and
  discontinuity sequences versus distinct run-specific playlist URIs. Recommend
  explicit run identity before switching HTTP readers; coordinate the identity
  decision with [HLS discontinuity](/quest/m1/release-audit/hls-discontinuity.md)
  and its [draft PR6426](https://github.com/moq-dev/moq/pull/6426), without
  importing that independent implementation. Failed timelines must retain their
  existing no-store 5xx behavior.
- FLV/RTMP and WebRTC listen and push outputs still need an explicit restart
  contract. Recommend remaining pinned until the old route dies unless their
  native output protocol can mark a new independent timeline. Decide whether
  to reconnect the output or mark the break in-session, then implement and test
  real publisher replacement without silently resetting timestamps or codec
  state. This checkpoint does not choose that contract.

The goal remains open until these remaining readers and shared-operation
choices are implemented and verified. Do not import the unmerged resolution
or announcement-owner proposals to obtain another API.

Public API: none. Wire: none. CLI lifetime/refusal behavior changes as above.
