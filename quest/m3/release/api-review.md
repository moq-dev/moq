# [L] API shape review before the cut

## Goal

Every public API added or changed since the last release, in Rust, JS, the
FFI, and every binding, has been reviewed for a better shape, and each change
the maintainer approves is a quest that lands before the cut.

## Plan

Recurs once per API or wire cut (decided 2026-10-19; per breaking cut
2026-11-09). The [release cut](/quest/m3/release/cut.md) re-creates this quest
when the maintainer names the next API or wire cut, so the PR finishing this
run adds no successor.
Record what this run learned (inventory method, findings, what to check first)
in the cut's PR notes or a follow-up quest, so the next run starts from it.
Only `/quest-delete` ends the recurrence.

Starts once m2 lands, so the APIs it reviews are the ones that ship; the cut
waits for it. API shape is the maintainer's call (decided 2026-10-19), so this
run changes no code. Inventory the diff against the last published version
by running `just api-diff`
locally against `main`, since its CI report only appears on the cut PR, and
hand diff the surfaces it cannot see. For each export that is awkward, judge it
against AGENTS.md's Public API rules: names mirrored across languages, no
callbacks or 4+ args, misuse unrepresentable, private until a consumer needs
it. Propose 2-3 shapes with a recommendation, ask the maintainer, and file
each approved change as a quest in m2, which holds the cut until it lands.
Model items only the built-in sessions use stay private
([moq-dev/moq#6747](https://github.com/moq-dev/moq/pull/6747)); this review judges the shape of what ships.

This run's first pass, ahead of m2 (2026-11-07, maintainer, /quest-plan):
an audit of `release` (a005c7ad9a) against `main` (a9d7925123) plus the open
quests. `just api-diff` gave the mechanical inventory; nine read-only
reviewers (net model, net session, transport, media formats, native media,
apps, JS core, browser media, bindings) scored 127 clusters out of 10 for
performance, usability, testability, and future fit, and searched for
consumers in this repository and its downstream applications. Mean 6.9; three
clusters were restrictive (the native playout clock, JS duration units, and
moq-video's environment knobs). Its approved changes are m2 quests
(route-roles, js-time-types, mux-sync, no-sentinels, video-devices,
gateway-ingest (landed), and the api-shape epic (landed)),
two m1 quests, and plan edits. Check first, next run: surfaces `just
api-diff` cannot see (GStreamer properties, CLI flags and env, MIGRATE drift)
and publish versions of packages exempt from the cut's version. `just
api-diff` compares against this repository's `release` branch, which never
held `@moq/qmux`, so compare `js/qmux`'s candidate API against npm's newest
`@moq/qmux` (0.3.4 today, from moq-dev/web-transport) by hand, and the Rust `qmux`
crate against crates.io's newest `qmux` (0.6.6 today) the same way. Run
this comparison only after both web-transport publishers are frozen
([qmux-js-retire](/quest/m3/release/qmux-js-retire.md),
[qmux-rs-retire](/quest/m3/release/qmux-rs-retire.md)), so the newest is
the last; if a freeze lands later, repeat it.
This review still runs after m2 lands, over what changed since.

The [feature review](/quest/m3/release/feature-review.md)'s first pass
(2026-11-14) found exports whose consumer never came or whose premise was
later removed; judge each here. Recommended: drop each unless a consumer turns
up.

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): ✅ every
unused public item is removed or hidden before the cut, in
[one sweep per area](/quest/m2/unused-api/README.md), each re-checked for a
downstream caller first (rejected: judging them after m2 lands). That audit
re-read this list, so it is settled:

- moq-ffi's `MoqTrackProducer::largest` and `MoqBroadcastConsumer::closed`
  stay: both have callers (the first pass's claim fell on a re-read).
- The byte-stream rung of `MoqMediaProducer`'s simulcast ladder stays:
  removing it changes `lower`'s type in every binding again.
- `moq_mux`'s `set_wait` folds into `Delay`
  ([the hole wait](/quest/m2/delay-hole-wait.md)). The first pass was wrong
  that native audio sets it to the `min` it already passes: it passes its
  live estimate, which that quest gives a home.
- `Rendition::applied()` and `moq_video::encode::Applied` stay: the media
  sweep found moq.pro's transcode bench reading them. moq-sock's ICMP error
  queue and `qmux::Config::fixed_receive_window` went in the transport sweep,
  [moq-dev/moq#8957](https://github.com/moq-dev/moq/pull/8957).
- Already gone: `TcpTls::new`
  ([moq-dev/moq#8852](https://github.com/moq-dev/moq/pull/8852)), and the
  `p2p` and `stun` features
  ([moq-dev/moq#8771](https://github.com/moq-dev/moq/pull/8771)).

What that audit learned, for the run after m2 and the next cut's:

- `just api-diff` lists breaks only for Rust, and nothing for a surface it
  marks `new`. Most of what the audit changed was in surfaces never
  published, whose whole API is free to change: diff their source by hand.
- Ask of each export who calls it outside its crate and tests, with the
  grep in the finding: the sweep's four children list what had none. Kixel
  and moq.pro pin `main` and call API nothing here does, so check them too.
- Shapes that recurred: a sentinel inside a value (a huge number for "no
  limit", a null that means zero in one record and unlimited in another),
  a closed enum the bindings switch over exhaustively, two names for one
  thing across Rust and JS, and a config key the code reads in two places.
- Judge what is absent as well: a missing piece that would break the shape
  when added later (a listing with no kind, a catalog with one version for
  every entry) is cheaper now than a removal is later.

## Required

- [Breaking release readiness](/quest/m2/README.md) - the release's code is final before it is reviewed
