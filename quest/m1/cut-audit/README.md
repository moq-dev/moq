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
drafts 14-17 removal (since landed), and
[buffered playback](/quest/m1/buffered-ascending.md). The audit recommends
leaving JS-only redesigns of `js/net` to [rs2ts](/quest/m2/rs2ts/README.md).

## Required

- [Counted field-less `trun` entries](/quest/m1/cut-audit/fmp4-sample-resolver.md) - a moof of field-less runs cannot materialize millions of sample entries
- [A reserved group survives a route returning](/quest/m1/cut-audit/reserved-route-return.md) - a route flip A to B to A no longer panics a peeked reader
- [A description-less AAC rendition means one thing](/quest/m1/cut-audit/aac-description-less.md) - every player and exporter agrees on the frame format
- [The export clock keeps only the rules admission still needs](/quest/m1/cut-audit/jitter-admission.md) - the clock rules the time and byte admission bound made redundant are measured and removed
- [Stream-accept loops yield on a per-poll budget](/quest/m1/cut-audit/accept-budget.md) - a peer opening many streams cannot monopolize a session driver
- [Rust and JS accept the same catalogs](/quest/m1/cut-audit/catalog-parity.md) - one strictness, per rendition, with a shared fixture corpus

## Related

- [Release audit findings](/quest/m1/release-audit/README.md) - the 2026-10-22 audit this one follows
- [Untrusted media parsers have fuzz targets](/quest/m2/media-fuzz.md) - would have caught the media parser findings here
- [Announce interest streams share one route table](/quest/m2/announce-interest-memory.md) - the audit's inherited announce memory finding
- [A parked read of a resumed group reads its budget without re-syncing](/quest/m2/recover-budget.md) - measure-first resume cost
- [Transcode rungs scale off the runtime](/quest/m2/transcode-rungs.md) - the audit's transcode findings
