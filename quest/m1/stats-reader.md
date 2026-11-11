# [S] JS reader for relay stats

## Goal

A new `@moq/stats` package under `js/stats` reads the relay's `moq-stats`
broadcasts the way `moq-stats`' Rust `Consumer` does: it discovers nodes
announced under a stats prefix, subscribes to one node's broadcast, follows
its track listing to each new run, and yields typed frames for every track
the Rust consumer reads. The demo dashboard (`demo/web/src/stats.ts`) reads
through it rather than parsing frames itself. Not here: hang's media stats
and viewer feedback, whose types stay in `@moq/hang`
([stats](/quest/m3/stats/README.md)), and any change to the wire format.

## Plan

Decided 2026-11-11 (maintainer, kixel.ai `/quest-unblock`): ✅ a new
`@moq/stats` package (rejected: exporting the reader from an existing package,
kixel.ai writing its own reader). ✅ relay `moq-stats` format only (rejected:
media stats too). ✅ m1 (rejected: m2, m3). Reason: kixel.ai's fleet page
reads both devices' and relays' stats in this format and should share one
reader with the dashboard rather than fork 800 lines that drift from the
Rust types. This amends the [stats](/quest/m3/stats/README.md) epic's "No
`@moq/stats` package" decision: the package exists, for the relay format
alone.

Mirror the Rust frame types (`TotalsFrame`, `PresenceFrame`, `PrefixFrame`
and the bucket snapshot) in TypeScript, and read snapshots with
`@moq/json`'s merge-patch reader. Keep the demo's legacy-name fallback out of
the package unless a still-supported relay needs it. Test against stubbed
tracks: discovery of two nodes, a run change that the listing follows, and a
merge-patch delta applied to a snapshot. Document it in
`doc/concept/stats.md` beside the Rust consumer.

## Related

- [Stats](/quest/m3/stats/README.md) - hang's media stats, which stay in `@moq/hang`
