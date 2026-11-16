# Socket endpoints leave moq for Kixel

## Goal

The cut publishes no socket endpoint API. `moq_net::socket`, `@moq/net`'s
`Socket`, and the socket handles of moq-ffi, moq-c, and every wrapper are
gone, with their concept page and interop lane, and the two demos that used
them, MoQ Boy and CARLA, send input the way they did before sockets: the
viewer publishes a broadcast and the publisher follows it. Kixel, the one
downstream that builds on sockets, carries its own copy on moq-net's public
API.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): ✅ revert
the socket endpoint and put it in Kixel, and go back to the old wire format
for CARLA and MoQ Boy (rejected: confirming sockets as a moq feature, which
was the audit's recommendation; keeping them and trimming the leftover
`_track` vocabulary). AGENTS.md puts work a large live-media platform would
not use in Kixel. Byte streams and the sockets' `tcp` protocol already moved
there on 2026-11-14
([moq-dev/moq#8772](https://github.com/moq-dev/moq/pull/8772)), which left
one downstream consumer, two demos, and a wrapper in every binding.

Facts found by the audit:

- Sockets are a naming convention over public API: a hidden broadcast per
  endpoint (`<prefix>/.server/<id>`, `<prefix>/.client/<id>`) and a track
  each way per session. `rs/moq-net/src/model/socket.rs` (1,150 lines) uses
  one crate-private call, `reserve_unlisted_track`
  (`rs/moq-net/src/model/broadcast.rs:406`).
- The surface that leaves: `js/net/src/socket.ts` (541 lines, 694 of tests),
  `rs/moq-ffi/src/socket.rs` (318), `rs/moq-c/src/socket.rs` (511), a wrapper
  in Python, Swift, Kotlin, Dart, and Go, `doc/concept/socket.md` (225), and
  `test/interop/socket.sh` with four clients.
- It was built by the m1 socket epic between 2026-10-28 and 2026-11-15:
  [moq-dev/moq#7677](https://github.com/moq-dev/moq/pull/7677),
  [#7883](https://github.com/moq-dev/moq/pull/7883),
  [#7884](https://github.com/moq-dev/moq/pull/7884),
  [#8115](https://github.com/moq-dev/moq/pull/8115),
  [#8468](https://github.com/moq-dev/moq/pull/8468),
  [#8741](https://github.com/moq-dev/moq/pull/8741),
  [#8825](https://github.com/moq-dev/moq/pull/8825), and the demo ports
  [#8008](https://github.com/moq-dev/moq/pull/8008) and
  [#8011](https://github.com/moq-dev/moq/pull/8011). None of it is on
  `release`, so nothing published breaks.

Order: the two demos move first, each in its own PR, and
[the removal](/quest/m2/sockets/remove.md) lands once nothing in the
repository dials or accepts. Kixel ports the Rust endpoint from history, from the
removal PR's parent commit, in [its own quest](https://github.com/kixel-ai/kixel.ai/blob/main/quest/m0/mesh/sockets.md),
and needs only `reserve_unlisted_track` public; it ports no JS.

## Related

- [moq.dev's hosted demos run the cut](/quest/m3/release/demos-deploy.md) - deploys the demos these children restore
