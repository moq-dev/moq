# m0: immediate priorities

## Goal

The work in flight now, in four independent tracks. Relay hardening: legal
moq-transport input never fails a session ahead of Seattle interop on
2026-10-12, every resource a peer can make the relay hold is bounded by what
it sent or by a budget, and no peer input panics the process. Routing: a
service claims the prefix it could serve instead of enumerating broadcasts.
Identity: nothing treats who published a route as what it carries; a path,
with its `@epoch`, is the only content identity, and every first-party
publisher that can restart mints a fresh epoch, so #4741 stalls nobody.
Audio playout: the target is a measured estimate of arrival timing in both
languages, and a browser regression fails a nightly run.

## Plan

The release API gates (#3829..#3878) and the release that followed them are
done. moq.pro pins this repository's `release` line, so the release gate below
also keeps #4741 from reaching it early.
The Pronto GPU integration lives in moq.pro.

The branch flip is done except its #4605 backport to `release`; the
Cloudflare switch waits on the maintainer as a condition quest. Relay
hardening: [REQUEST_OK accepts LARGEST_OBJECT](/quest/m0/ietf-largest-object.md)
leads the rest, since it is the one known case left of legal moq-transport
input closing a session, which Seattle would hit. The 2026-10-05 audit split
it from the m2 validator findings, which block nothing. The DoS hardening
from an external review on 2026-09-29, verified against `main`, stays in m0
as security work. Its quests describe fixes, not exploits.
[Stats linger](/quest/m0/stats-linger.md) joined m0 on 2026-10-05 as a
standalone quest, not a release gate: moq.pro's m0 waits on a `release`
commit carrying it, so it lands on main and is backported.

Routing: the wildcard line is prefix-only on the wire; its resolve and demand
work is done on the line branch and waits to land. Serving the relay's
ingested-only view (`origin::Consumer::local()`) to localhost workers belongs
to moq.pro's edge, which embeds moq-relay; it moved there on 2026-09-28.

Identity: the [broadcast epoch](/quest/m0/broadcast-epoch/README.md) line
ranks right after Wildcard and gates the next release (decided 2026-10-03:
#4741 resumes an un-epoched republish into the old broadcast and stalls its
viewers). #4741 can merge to main, but no release ships until first-party
publishers mint epochs.

Audio playout: the jitter target replaces the round-trip guess. The browser
audio quality harness in `test/audio-quality/` has landed; it grades playout
nightly and records the traces it replays. Its native lane is a standalone m1
quest, since nothing here waits on it. The [A/V clock](/quest/m1/av-clock.md)
moved to m1 in the 2026-09-30 audit: it waits on the whole jitter line and is
a published `@moq/watch` break.

## Required

- [Branch flip](/quest/m0/branch-flip.md) - `dev` becomes the default `main` trunk and today's `main` becomes `release`, where publishing runs
- [Cloudflare builds track release](/quest/m0/cloudflare-release.md) - condition: the maintainer points the docs and demo builds at `release`
- [REQUEST_OK accepts LARGEST_OBJECT](/quest/m0/ietf-largest-object.md) - a conformant moq-transport peer's REQUEST_OK no longer closes the session, ahead of Seattle
- [Request caps](/quest/m0/request-caps.md) - lite message sizes, IETF request IDs, and per-session announces and subscriptions are bounded
- [qmux credit](/quest/m0/qmux-credit.md) - qmux returns connection credit for dropped and stopped streams and delivers its close frame, on both lines
- [Shared fronts](/quest/m0/shared-fronts.md) - viewer sessions share a front, so fronts scale with peers, not viewers, and viewer churn no longer leaks fronts
- [Stats linger](/quest/m0/stats-linger.md) - a grouped stats broadcast stays announced for a linger after its last session, so viewer churn stops re-announcing it across the mesh, backported to `release`
- [Wildcard](/quest/m0/wildcard/README.md) - a relay resolves subscriptions against advertised prefixes, a service claims the prefix it could serve and refuses the rest instead of enumerating broadcasts, and the browser player treats a covering claim as availability
- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - every first-party publisher that can restart mints a fresh `@<uuidv7>` epoch, viewers follow the newest live one, and bare names still resolve on every version
- [Audio jitter target](/quest/m0/audio-jitter-target/README.md) - the audio playout target is a measured estimate of arrival timing in both languages, not a round-trip guess

## Related

- [Pronto GPU integration](https://github.com/moq-dev/moq.pro/tree/main/quest/m0/pronto/gpu) - CARLA bridge, release adoption and desktop installation
