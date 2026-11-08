# [L] Minted instances are a typed flag

## Goal

Whether a session minted an instance is a typed property of `Instance`, not
an in-band `~` prefix on its text:

- `moq_net::Instance` carries a minted flag that the public API cannot set.
  `FromStr` and `Instance::random()` build only named instances; only a
  session mints one, for a route whose wire (moq-lite 06 or older,
  moq-transport) carries none. `is_minted()` reads the flag. A named
  instance may start with `~` like any other text.
- @moq/net mirrors it: `Instance.isMinted` reads the flag, and
  `Instance.parse` / `Instance.random` build only named instances.
- moq-lite-07 (still the opt-in `moq-lite-07-wip`) carries an explicit
  minted flag next to the instance wherever it carries one: the route an
  announcement serves, and the instance a TRACK, TRACKS, SUBSCRIBE, FETCH,
  or DATAGRAMS request pins. The draft drops the `~` convention.
- Minting itself stays, and so does what "minted" means: an older-wire route
  gets one id shared across the lite-07 mesh (dedupe, agreed ranking), but no
  continuity promise. A minted instance is never re-sourced, a catalog's
  instance check against it is best effort, and signing refuses to bind it.

## Plan

Decided 2026-11-08 by the maintainer, replacing api-shape/net's "should a
route refuse a minted instance" item: the `~` prefix is a design flaw. It is
an in-band sentinel in a free-form string, so any caller can spoof a minted
instance by naming one `~...`, and a real name starting with `~` is
ambiguous. ✅ Typed flag that only a session sets, carried explicitly on the
lite-07 wire (rejected: refuse a minted instance in `route`, keep accepting
it with docs, refuse in Rust only). Once the API cannot build a minted
instance, `Producer::route` and `Request::route` need no refusal.

Call sites (`git grep -n 'is_minted\|isMinted\|MINTED'` over `rs/` and
`js/`, plus `Instance::minted` / `Instance.minted`):

- Type: `rs/moq-net/src/instance.rs` (`MINTED`, `minted()`, `is_minted()`,
  `FromStr`, `Display`, tests), `js/net/src/instance.ts` (`MINTED`,
  `minted()`, `isMinted()`, `parse`). JS `Instance.Valid` is a branded
  string used as a map key and compared with `===` in about 34 files, so the
  JS shape (an object with a stable key, or a string keyed form plus a flag)
  is the main judgment call; pick one that keeps equality and map keys
  correct and say why in the PR.
- Minting: `rs/moq-net/src/lite/subscriber.rs`,
  `rs/moq-net/src/ietf/subscriber.rs`, `js/net/src/lite/subscriber.ts`,
  `js/net/src/ietf/subscriber.ts`.
- Wire: `rs/moq-net/src/lite/instance.rs` and `js/net/src/lite/instance.ts`
  (route and pin encode/decode), `drafts/draft-lcurley-moq-lite.md` (the
  `~` paragraph near "A version before this one carries no Instance", and
  the lite-07 changelog line on minting), and the lite corpus fixtures.
- Readers of the flag: `rs/moq-net/src/model/origin.rs`,
  `rs/moq-crypto/src/sign/broadcast.rs`,
  `js/crypto/src/sign/broadcast.ts`, `rs/moq-mux/src/source.rs`,
  `rs/moq-mux/src/follow.rs`, `rs/moq-gst/src/source/imp.rs`,
  `js/net/src/consume.ts`, `js/net/src/origin.ts`, `js/watch/src/broadcast.ts`.
- Tests that route a minted instance directly
  (`moq-mux` `follow::tests::a_named_instance_follows_a_minted_one`,
  @moq/watch `broadcast.test.ts` and `include.test.ts`) can no longer build
  one through the public API; give them a minted route over a pre-lite-07
  session or a test-only constructor, whichever is simpler.
- Docs: `doc/concept/identity.md` ("A minted instance starts with `~`"),
  `doc/concept/moq-lite.md` ("marked with a leading `~`"),
  `drafts/draft-lcurley-moq-hang.md` and `drafts/draft-lcurley-moq-sign.md`
  where they cite the `~` mark, `rs/moq-net/MIGRATE.md`, and
  `js/net/api/index.api.md`.

This changes lite-07 bytes, so it joins
[Finalize moq-lite-07](/quest/m2/lite07-finalize.md)'s Required.
[moq-dev/moq#8220](https://github.com/moq-dev/moq/pull/8220) also changes
lite-07 (SUBSCRIBE_DROP and SUBSCRIBE_SKIP ranges) and touches
`lite/subscriber.rs`, `lite/subscribe.rs`, their JS mirrors, and the lite
draft, but not the instance encoding; expect textual conflicts only, and
whichever lands second adds its changelog line beside the other's.

## Related

- [moq-net's route stream goes public](/quest/m2/api-shape/net.md) - the
  audit item this replaces
