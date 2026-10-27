# Pre-cut audit findings

## Goal

Resolve the bugs and hostile-input costs found by the 2026-10-24 pre-cut
audit of main before main is promoted to release. This epic's own work, once
every child has merged: re-run the reproductions the children record against
the release candidate and hand the result to the
[release cut](/quest/m2/release/cut.md).

## Plan

The maintainer asked for an extensive audit of main for bugs and
improvements before the cut, with attention to high-churn areas that need a
redesign. The audit ran at
[151a529670](https://github.com/moq-dev/moq/commit/151a529670) in eight
areas: the moq-net track and resume model, origin and sessions with kio,
moq-transport, moq-lite with its codecs and fuzzing, `js/net` with
`js/qmux`, moq-mux with hang in both languages, the browser media packages,
and the runtimes and binaries (moq-tokio, moq-uring, the relay's non-auth
paths, moq-cli, moq-rtmp, moq-gst, moq-transcode, moq-audio, moq-video). It
skipped what the 2026-10-22 to 24 audits already covered (release-to-main,
relay security, clustering, linger, moq-quic, moq-hls, the external API
audit, bindings and OBS), and every finding was checked against the quest
tree and the open PRs.

This was risk-based source review with targeted reproductions in throwaway
tests, not a line-by-line pass; TS import/export beyond spot checks was not
covered. Each child says whether its finding was reproduced or
source-inferred and whether it is new on main or inherited from release.
Children are ordered by severity: remotely reachable crashes and
amplification first, then correctness and privacy, then bounded costs and
cleanups.

Churn context: main landed about 2,400 commits in the 30 days before the
audit, 823 of them fixes, concentrated in `rs/moq-net` (`model/track.rs` 85
fix commits, `ietf/subscriber.rs` 69, `ietf/publisher.rs` 53,
`lite/subscriber.rs` 51, `model/resume.rs` 46) and `js/net`. The audit's
redesign proposals for the churning mechanisms are in m2, since each is
internal and can ship after the cut:
[resume as producer replacement](/quest/m2/resume-redesign.md),
[moq-transport request leases](/quest/m2/ietf-request-lease.md),
[group heads filled in place](/quest/m2/ietf-fill-refill.md),
[one relay serve path](/quest/m2/relay-serve-path.md),
[moqsrc pad actors](/quest/m2/moqsrc-pad-actor.md), and
[publish rendition identity](/quest/m2/publish-publication.md). Other
redesign evidence went into the quests that already own the mechanism:
[subscribe ranges](/quest/m1/subscribe-ranges/README.md),
[drop set](/quest/m1/drop-set.md),
[per-origin routes](/quest/m1/cluster-routing/routes.md),
[drafts 14-17 removal](/quest/m1/ietf-drop/d14-17.md), and
[buffered playback](/quest/m1/buffered-ascending.md). The audit recommends
leaving JS-only redesigns of `js/net` to [rs2ts](/quest/m2/rs2ts/README.md).

## Required

- [One fMP4 sample resolver](/quest/m1/cut-audit/fmp4-sample-resolver.md) - zero-size samples cannot cost gigabytes, and `base_data_offset` input imports or is refused
- [A compressed announce path never outgrows its literal form](/quest/m1/cut-audit/announce-path-bytes.md) - lite-07 announce compression cannot build multi-MiB paths
- [A route too large to re-encode is skipped](/quest/m1/cut-audit/announce-reencode.md) - one oversized route never resets a downstream announce stream
- [A reserved group survives a route returning](/quest/m1/cut-audit/reserved-route-return.md) - a route flip A to B to A no longer panics a peeked reader
- [JS tracks end clean only on a deliberate finish](/quest/m1/cut-audit/js-close-cause.md) - local close, violations, and unanswered requests end with errors
- [A malformed lite message closes the session](/quest/m1/cut-audit/announce-decode-complete.md) - a bad ANNOUNCE body no longer jams its stream
- [Withdrawing a moq-transport FETCH or TRACK_STATUS ends it](/quest/m1/cut-audit/ietf-withdraw-resolve.md) - withdrawal during resolution releases the request
- [JS moq-transport sessions close on peer violations](/quest/m1/cut-audit/js-ietf-violations.md) - right close codes, no unhandled rejection, bounded reflected namespaces
- [A media hole in the player keeps the timeline](/quest/m1/cut-audit/watch-audio-hole.md) - a skipped group re-primes decoders without flushing audio or re-anchoring sync
- [A camera broadcast stays announced across unmute](/quest/m1/cut-audit/publish-announce-latch.md) - viewers do not restart on every device flap
- [Muted and volume are independent](/quest/m1/cut-audit/watch-volume-mute.md) - `<moq-watch volume="0.8" muted>` starts muted at 0.8
- [A description-less AAC rendition means one thing](/quest/m1/cut-audit/aac-description-less.md) - every player and exporter agrees on the frame format
- [A malformed MKV track fails the import](/quest/m1/cut-audit/mkv-import-errors.md) - no silently missing tracks or collapsed timestamps
- [RTMP sessions follow one strict path](/quest/m1/cut-audit/rtmp-session-strict.md) - idle sockets, repeated requests, rewinds, and stalled servers are handled
- [The export jitter buffer admits only frames due within its window](/quest/m1/cut-audit/jitter-admission.md) - a fast track is backpressured, not buffered without bound
- [Peer-driven receive loops yield on a per-message budget](/quest/m1/cut-audit/peer-loop-budget.md) - small-message floods cannot monopolize a session driver
- [Removing a stale front is O(1)](/quest/m1/cut-audit/weak-cache-remove.md) - a restart wave is linear under the origin lock
- [One bad rendition never leaves the player with nothing](/quest/m1/cut-audit/watch-source-robust.md) - malformed audio and conflicting video filters degrade gracefully
- [@moq/flate stops inflating at its budget](/quest/m1/cut-audit/flate-budget.md) - unless the compression migration deletes it first
- [Media element attributes reflect their state](/quest/m1/cut-audit/element-attributes.md) - attributes track property and UI changes before the API ships
- [Rust and JS accept the same catalogs](/quest/m1/cut-audit/catalog-parity.md) - one strictness, per rendition, with a shared fixture corpus
- [Lite datagrams obey one size limit](/quest/m1/cut-audit/datagram-size.md) - no datagram one hop accepts is dropped by the next for size
- [Every relay runtime honors or refuses each listen setting](/quest/m1/cut-audit/uring-listen-config.md) - io_uring stops ignoring preferred addresses
- [The Unix-socket allowlist is checked first](/quest/m1/cut-audit/uds-allowlist-first.md) - refused local users never reach the qmux parser
- [A route's terminal state is one phase](/quest/m1/cut-audit/route-phase.md) - three resume flags collapse into one check
- [Lite's version gates, refusals, and draft text agree](/quest/m1/cut-audit/lite-wire-nits.md) - small wire and draft mismatches fixed
- [Small @moq/net divergences from Rust are fixed](/quest/m1/cut-audit/js-net-cleanups.md) - lite-04 probes, group consumers, accept deadlines, empty JSON

## Related

- [Release audit findings](/quest/m1/release-audit/README.md) - the 2026-10-22 audit this one follows
- [Untrusted media parsers have fuzz targets](/quest/m2/media-fuzz.md) - would have caught the media parser findings here
- [Announce interest streams share one route table](/quest/m2/announce-interest-memory.md) - the audit's inherited announce memory finding
- [A parked read of a resumed group reads its budget without re-syncing](/quest/m2/recover-budget.md) - measure-first resume cost
- [Transcode rungs scale off the runtime](/quest/m2/transcode-rungs.md) - the audit's transcode findings
