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
  child's Plan for code outside this repository. When a child completes before
  re-creation, move its checked heads and next-run decisions into this Plan.
  Step 2 re-creates completed children from those records too.

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

UniFFI recurrence baseline, checked 2026-11-02: all four upstream default
branches were checked, and all four public advisory lists were empty. Carry
these checked heads into the next child:

| Upstream | Head checked |
|---|---|
| [mozilla/uniffi-rs](https://github.com/mozilla/uniffi-rs) | [`ef9625b6`](https://github.com/mozilla/uniffi-rs/commit/ef9625b68dd51207f55f024a27649e58edb52b09) |
| [NordSecurity/uniffi-bindgen-go](https://github.com/NordSecurity/uniffi-bindgen-go) | [`0b7fb4ce`](https://github.com/NordSecurity/uniffi-bindgen-go/commit/0b7fb4ceef12021bd7f790cc516fa9133e001813) |
| [Uniffi-Dart/uniffi-dart](https://github.com/Uniffi-Dart/uniffi-dart) | [`e644a4ad`](https://github.com/Uniffi-Dart/uniffi-dart/commit/e644a4ad980f72697aba211fd30cb4fbd8fc7324) |
| [livekit/uniffi-bindgen-cpp](https://github.com/livekit/uniffi-bindgen-cpp) | [`20bef754`](https://github.com/livekit/uniffi-bindgen-cpp/commit/20bef754391023df147267482513c35120dee343) |

MoQ exports no foreign-implementable traits, custom types, dollar-bearing
defaults, or recursive enum graphs. Recheck these uses before importing the
related upstream fixes. Upstream's parser, JNI backend, mutable bytes, and
library-mode removal are redesigns outside this sync. Before changing Dart
fork CI, read its [coverage record](https://github.com/kixelated/uniffi-dart/blob/moq-0.32/README.md#fork-ci-coverage).
Until the exporter switch, mirror any generator fixes into rust2's maintenance
fork too and add them to the exporter quest's carried-over list, so the new
generators retain them. rust2's fork is maintenance-only since 2026-11-02.
Drop the UniFFI child once
[the exporter switch](/quest/m2/ffi-exporter.md) lands.

Out of scope: forks moq no longer consumes (moq-dev/quiche, moq-dev/noq,
kixelated/cros-codecs, kixelated/openh264-rs, kixelated/fflate) and the
`@kixelated/libavjs-webcodecs-polyfill` npm fork js/hang uses (maintainer,
2026-10-30: its upstream has not changed since 2025-07).

Public API: none expected; a ported fix that changes one says so. Wire: QUIC
behavior follows quinn's fixes.

## Required

- [moq-vaapi has every cros-codecs fix it needs](/quest/m1/vendor-sync/vaapi.md) - cros-libva and cros-codecs fixes reach moq-dev/vaapi

## Related

- [Dependency audit](/quest/m2/audit-deps.md) - covers crates cargo-deny can see
