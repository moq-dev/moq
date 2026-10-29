# [S] moq-c announces through its own route struct

## Goal

A C publisher announces with a publish-only route that matches moq-ffi's
`MoqAnnounceRoute`: a cost and an optional epoch, so it can announce a restart
and viewers see a restart instead of a stalled broadcast. It can no longer
claim relay hops it never took. `moq_route` stays for received routes, where
hops are real.

## Plan

Found while landing [moq-dev/moq#7044](https://github.com/moq-dev/moq/pull/7044),
which moved announcing onto `moq_publication_announce`. Today that call takes
`moq_route` (`rs/moq-c/src/api.rs`), which has no epoch and accepts any
caller-supplied hops, while moq-ffi's `MoqAnnounceRoute`
(`rs/moq-ffi/src/origin.rs`) leaves hops to the origin.

Decided with the maintainer on 2026-10-28:

- **A separate announce struct**, `moq_announce_route { cost, epoch }`,
  mirroring moq-ffi, rather than refusing non-empty hops on the shared
  `moq_route` or validating hop values. `moq_route` keeps describing received
  routes.
- **Epoch as UUIDv7 text**, as moq-ffi's `epoch`, minted the way
  `moq_mint_epoch` is in moq-ffi; add a C mint call if one is missing.
- **In m1:** the hand-written moq-c ships at the cut with parity with moq-ffi
  for every m1 binding feature (decided 2026-10-25), so this C ABI break lands
  before it.

Update the C header, `c/moq`'s probe if it announces, the C++ and OBS
wrappers, `doc/lib/c`, and the tests: a restart epoch reaching a subscriber as
a restart, and no hops on a locally announced route.

Public API: breaking in the C ABI (`moq_publication_announce` takes the new
struct), with an Impact migration note. Wire: none.
