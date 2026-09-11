# [XL] Merge dev into main

## Goal

`main` carries everything on `dev`: the thread-per-core runtime, the net model
and allocator work, the announce handle and its bindings, the archive catalog
and store, and every fix that only dev has. The merge PR closes each issue dev
fixed with a closing keyword, and the release that follows is the one moq.pro
adopts.

## Plan

main is merged into dev as of 2026-09-12, so the merge PR opens from dev with
the list below as closing keywords once the gates clear. Run `just check-all`,
`just test all`, `just test smoke-full`, and `just bench origin/main` on the
merged tree, and record the revision the proof ran on.

Resolve by behavior, never by side. The traps the last main-into-dev merge
hit, so the merge PR re-checks each on the combined tree:

- main's joining FETCH keeps its saved object prefix (#3562) and answers with
  FETCH_OK; dev's registry maps its two refusal codes to DOES_NOT_EXIST until
  [IETF leftovers](/quest/m2/ietf-leftovers.md) adds them.
- main's refusal of an incompatible copy during failover (#3521) lives on
  dev's source table (`FrontState::track_info`, `accept_track_info`) and is
  checked on `Step::Splice`; a successor with a different timescale never
  rescales samples a subscriber already holds.
- dev's structural flush-span jitter (#3513), registered request codes
  (#3531), bounded `moq_json::window` timeline (#3240), and duration marker
  (#3575) keep their regressions; main's draft-21 (#3574) is in every
  per-version list, including the stream and request registries.
- The hop-0 ban is abandoned (#3623); anonymous routes rank last instead
  ([Anonymous rank](/quest/m1/anonymous-route-rank.md)).
- Auto-merged files hide breakage: `just check` and `just test` run before
  the merge commit, not after.

Also soak HLS on the merged tree: a fresh viewer joining a `moq import ts`
broadcast that has been up for days must get a playlist promptly. The bounded
`moq_json::window` timeline (#3240) is what makes that hold, and only a long
run proves it.

The breaking-change targeting rules in CONTRIBUTING.md govern the release
that follows. Dart lands on that surface with #3190, so nothing waits on this
merge for it. The rest of the archive line, wildcard resolution, and every
additive quest that builds on dev-only code start on main afterwards from
[m2](/quest/m2/README.md).

## Required

- [m0](/quest/m0/README.md) - every release blocker lands or is punted before the merge
- [#3190](/quest/m1/3190-align-origin-broadcast-creation-naming-across-language.md) - every native binding on the announce surface
- [Monotonic timeline](/quest/m1/monotonic-timeline.md) - so a shed marker still jumps the playhead on a timestamp hole (#3291)
- [Advertise](/quest/m1/wildcard-advertise.md) - so `dynamic(prefix, route)` takes a path pattern before the announce API is published

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
- [#679](https://github.com/moq-dev/moq/issues/679) - QUIC receive is spread across thread-per-core workers, each on its own socket (#2921)
- [#1073](https://github.com/moq-dev/moq/issues/1073) - the origin lifecycle is caller-driven: `origin::Driver` plus `moq_tokio::origin::spawn` (#2897, #2901)
- [#2155](https://github.com/moq-dev/moq/issues/2155) - js/net: subscriptions take a `Subscription` options object with `startGroup`, `endGroup`, and `update()` (#2716); ordering became a handle (#3099)

## Related

- [Archive](/quest/m2/archive/README.md) - the writer, reader, HLS, DVR, browser, and proof follow the release
- [Wildcard](/quest/m2/wildcard/README.md) - Resolve and Demand are additive and follow the release
