# [L] Merge dev into main

## Goal

`main` carries everything on `dev`: the thread-per-core runtime, the net model
and allocator work, the announce handle and its bindings, the archive line, and
every fix that only dev has. The merge PR closes each issue dev fixed with a
closing keyword, and the release that follows is the one moq.pro adopts.

## Plan

As of 2026-09-05 dev is 243 commits ahead of main and main 16 ahead of dev.
Merge main into dev first and resolve there, then open the merge PR from dev
with the list below as closing keywords. Run `just check-all`, `just test all`,
`just test smoke-full`, and `just bench origin/main` on the merged tree. The
breaking-change targeting rules in CONTRIBUTING.md govern the release that
follows, and [Dart announce](/quest/m1/dart-announce.md) unblocks when this
lands.

## Required

- [Archive](/quest/m1/archive/README.md) - moq.pro needs archive-backed recording on the release dev produces
- [Announce handle](/quest/m1/announce-handle.md) - the origin surface the merge ships
- [#3190](/quest/m1/3190-align-origin-broadcast-creation-naming-across-language.md) - every native binding on that surface
- [JS announce](/quest/m1/js-announce.md) - js/net on that surface
- [Gap discontinuity](/quest/m1/gap-discontinuity.md) - so the lost-reset regression does not ship

## Closes

- [#3173](https://github.com/moq-dev/moq/issues/3173) - moq-uring: worker drop cancels staged receives (8acbeb306)
- [#3107](https://github.com/moq-dev/moq/issues/3107) - moq-relay: the io_uring listener serves its certificate fingerprint (#3116)
- [#3112](https://github.com/moq-dev/moq/issues/3112) - moq-relay: the io_uring listener authenticates mTLS peers (#3116)
- [#3160](https://github.com/moq-dev/moq/issues/3160) - moq-tokio: a rustls backend without a crypto provider is a compile error (#3210)
- [#3119](https://github.com/moq-dev/moq/issues/3119) - moq-uring: UDP pools grow when a socket starves (#3133)
- [#3111](https://github.com/moq-dev/moq/issues/3111) - moq-uring: an unmappable HTTP/3 code lands in `Error::Http3` (#3110)
- [#2627](https://github.com/moq-dev/moq/issues/2627) - js: a detached element's connection lingers for a real window (#2705)
- [#2628](https://github.com/moq-dev/moq/issues/2628) - js: one connection per relay URL (#2705)
- [#2532](https://github.com/moq-dev/moq/issues/2532) - js/publish: files are demuxed and decoded, not captured (#2541)
- [#2609](https://github.com/moq-dev/moq/issues/2609) - moq-ffi: sessions reconnect automatically (#2618)
- [#2807](https://github.com/moq-dev/moq/issues/2807) - js/net: a group pop and its frame-range snapshot are one step (#2820)
- [#2808](https://github.com/moq-dev/moq/issues/2808) - js/net: the bounds snapshot is atomic with the pop (#2820)
- [#2892](https://github.com/moq-dev/moq/issues/2892) - js/net: the subscriber latency budget is enforced (#2926)
- [#2517](https://github.com/moq-dev/moq/issues/2517) - net: subscriptions resume across routes sharing a first hop (#3312)
- [#2934](https://github.com/moq-dev/moq/issues/2934) - moq export ts: SI is emitted when the snapshot changes, repeats are floored (#2971)
- [#3049](https://github.com/moq-dev/moq/issues/3049) - moq-net: `Hops::push` enforces the loop-free chain wherever one is built (#3066)
- [#1238](https://github.com/moq-dev/moq/issues/1238) - js: standalone components default to enabled (#2872)
- [#3260](https://github.com/moq-dev/moq/issues/3260) - stream consumers detect a rolled log without waiting for the group to end (#3282)
- [#3174](https://github.com/moq-dev/moq/issues/3174) - js/publish: the gain target is set before the first frame (#2541)
- [#3171](https://github.com/moq-dev/moq/issues/3171) - js/publish: fanout queue limits are validated (7bb180fdc)
- [#3172](https://github.com/moq-dev/moq/issues/3172) - js/net: Reload stops after a terminal failure (#3177)
- [#2401](https://github.com/moq-dev/moq/issues/2401) - the route linger is gone; overlapping routes resume instead (#2704, #3312)
- [#2217](https://github.com/moq-dev/moq/issues/2217) - moq-ffi: the announce handle carries the lifecycle (announce-handle, #3190)
- [#980](https://github.com/moq-dev/moq/issues/980) - dual-stack binding on main; happy eyeballs in `moq-tokio::resolve` (#2749)
- [#2153](https://github.com/moq-dev/moq/issues/2153) - go: the wrapper caught up on main; hops landed on dev (#2168)

## Related

- [Dart announce](/quest/m1/dart-announce.md) - waits on this merge, so it cannot be required here
