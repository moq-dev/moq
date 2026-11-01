# Vendored code has every upstream fix it needs

## Goal

Once per release cycle, every library vendored into moq-dev/moq, plus
moq-dev/vaapi and the UniFFI generator forks, is compared against its upstream
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
  child's Plan for code outside this repository.

This README's own work, once every child has merged:

1. Scan the tree for code vendored since the last run: new crates or modules
   whose README, Cargo description, or module doc says forked, vendored, or
   ported from an upstream, and any new `git` dependency or flake input
   pointing at a fork. Each one gets a child in the next run, starting from its
   vendored commit. Code whose upstream is gone or frozen (moq-tokio's
   `quic::web`, from moq-dev/noq) is listed in the README it lives in and
   gets no child.
2. Re-create the questline at `/quest/m2/vendor-sync/`, linked from
   `/quest/m2/README.md`'s `Required`. Every child keeps its Goal and carries
   forward its last checked commit and what this run learned (what to check
   first, what was ruled out). Each child requires
   [the release cut](/quest/m2/release/cut.md), which removes itself from
   every `Required` and shifts m2 into m1, so the next run becomes ready in the
   new m1, once per release cycle.
3. Drop a child whose code was deleted or whose upstream stopped mattering,
   saying why in the PR.

Only `/quest-delete` ends the recurrence.

Out of scope: forks moq no longer consumes (moq-dev/quiche, moq-dev/noq,
kixelated/cros-codecs, kixelated/openh264-rs, kixelated/fflate) and the
`@kixelated/libavjs-webcodecs-polyfill` npm fork js/hang uses (maintainer,
2026-10-30: its upstream has not changed since 2025-07).

Public API: none expected; a ported fix that changes one says so. Wire: QUIC
behavior follows quinn's fixes.

## Required

- [moq-quic has every quinn fix it needs](/quest/m1/vendor-sync/quinn.md) - quinn's fixes since the last check are ported with their tests
- [moq-v4l has every libv4l-rs fix it needs](/quest/m1/vendor-sync/v4l.md) - libv4l-rs fixes since v4l 0.14 are ported or the API they fix is gone
- [UniFFI generator forks are current](/quest/m1/vendor-sync/uniffi.md) - the generator forks carry upstream's fixes until rust2 replaces them
- [moq-vaapi has every cros-codecs fix it needs](/quest/m1/vendor-sync/vaapi.md) - cros-libva and cros-codecs fixes reach moq-dev/vaapi
- [moq-nvenc has every upstream fix it needs](/quest/m1/vendor-sync/nvenc.md) - nvidia-video-codec-sdk fixes since the vendored commit are ported
- [moq-rtmp's rml has every upstream fix it needs](/quest/m1/vendor-sync/rml.md) - rust-media-libs fixes since rml_rtmp 0.8.0 are ported

## Related

- [Dependency audit](/quest/m2/audit-deps.md) - covers crates cargo-deny can see
