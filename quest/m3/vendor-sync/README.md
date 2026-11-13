# Vendored code has every upstream fix it needs

## Goal

Once per release cycle, every library vendored into moq-dev/moq, plus
the UniFFI generator forks, is compared against its upstream
from the last checked commit. Useful fixes and features are ported or planned,
each library records the newest upstream commit checked, and code vendored
since the last run gets its own child.

## Plan

Decided 2026-10-30 (maintainer). moq vendors upstream code in-tree rather than
keeping forks in separate repositories, and cargo-deny cannot see an advisory
in vendored code, so checking upstream is recurring work. One questline covers
it, with one child per upstream, so the children run in parallel.

Each child, each run:

- Compare upstream's default branch, plus its security advisories, against the
  last checked commit its Plan names.
- Port an XS security or correctness fix with its regression test in the
  child's PR, or record why it does not apply. Plan anything larger, and any
  upstream feature worth having, with `/quest-plan` as a follow-up. Skip
  dependency bumps.
- Record the newest upstream commit checked where the next run will look: an
  `Upstream` section in the vendored crate's README (or module doc), or the
  child's Plan for code outside this repository. When a child completes before
  re-creation, move its checked heads and next-run decisions into this Plan.
  Step 2 re-creates completed children from those records too.

This README's own work, once every child has merged:

1. Scan the tree for code vendored since the last run: new crates or modules
   whose README, Cargo description, or module doc says forked, vendored, or
   ported from an upstream, any `LICENSE`, `PATENTS`, or `Copyright` notice
   carried from another project, and any new `git` dependency or flake input
   pointing at a fork. Each one gets a child in the next run, starting from its
   vendored commit. Code whose upstream is gone or frozen (moq-tokio's
   `quic::web`, from moq-dev/noq) is listed in the README it lives in and
   gets no child.
2. Re-create the questline at `/quest/m3/vendor-sync/`. Every child keeps its Goal and carries
   forward its last checked commit and what this run learned (what to check
   first, what was ruled out). Each child requires
   [the release cut](/quest/m3/release/cut.md), which removes itself from
   every `Required` and moves the questline under the re-created
   cut gate (`quest/m2/cut-gate/`) in m2, so the next run becomes
   ready there, once per release cycle (rule replaced 2026-11-05).
3. Drop a child whose code was deleted or whose upstream stopped mattering,
   saying why in the PR.

Only `/quest-delete` ends the recurrence.

The 2026-11-05 scan found two new upstreams, each now a child: WebRTC
(moq-congestion's GoogCC and moq-audio's NetEq time stretch) and SCReAMv2
(moq-congestion's `scream`). The NetEq and SCReAMv2 ports recorded no upstream
commit, so their first runs establish one. moq-congestion's Cubic came from
quinn and stays with the quinn child.

The 2026-11-12 audit found moq-webrtc's vendored str0m, dimpl, `is`, and
sctp-proto (vendored 2026-11-07, after that scan) with no child, so they got
[one](/quest/m3/vendor-sync/webrtc-stack.md) starting from the revisions
`rs/moq-webrtc/src/vendor/mod.rs` records.

Out of scope: forks moq no longer consumes (moq-dev/quiche, moq-dev/noq,
kixelated/cros-codecs, kixelated/openh264-rs, kixelated/fflate, and
kixelated/goog_cc, now in-tree as moq-congestion's `gcc`), the
`@kixelated/libavjs-webcodecs-polyfill` npm fork js/hang uses (maintainer,
2026-10-30: its upstream has not changed since 2025-07), and `@moq/libav`,
which builds pinned upstream libav.js, ffmpeg, and opus releases unmodified.

Public API: none expected; a ported fix that changes one says so. Wire: QUIC
behavior follows quinn's fixes.

## Related

- [Cost audit](/quest/m3/audit-cost.md) - covers crates cargo-deny can see
