# [S] Accept EXPIRES from IETF publishers

## Goal

A moq-net subscriber accepts the EXPIRES parameter (0x08) that aiomoqt and
libquicr send in SUBSCRIBE_OK, so a relay can subscribe upstream to either of
them. PUBLISH_OK and REQUEST_OK accept it too, in Rust and JS. What the
publisher sends is unchanged.

## Plan

- `SubscribeOk`, `PublishOk`, and `RequestOk` in `rs/moq-net/src/ietf/`
  decode 0x08 on every draft that uses parameters, then drop it. Today
  `decode_params!` rejects unlisted keys with `InvalidValue`. `PublishOk`
  already accepts the subscription parameters on draft 20, where the grammar
  moved them to PUBLISH; EXPIRES gets the same leniency rather than a
  draft-20-only rejection.
- EXPIRES is ignored on purpose. It is the wall-clock time until the publisher
  plans to end the subscription. That end already arrives as PUBLISH_DONE, and
  moq-net never refreshes a subscription through REQUEST_UPDATE. Retention is
  MAX_CACHE_DURATION's job; see [Optional max age](/quest/m1/ietf-max-age.md).
- Draft 14 carries expires as a fixed SUBSCRIBE_OK field and rejects non-zero
  values with `Unsupported` in Rust and in `js/net/src/ietf/subscribe.ts`.
  Accept and ignore it in both, and replace the rejection tests.
- JS already parses the parameter on later drafts
  (`js/net/src/ietf/parameters.ts`).
- Regression tests: decode the two captured SUBSCRIBE_OK bodies from #4172,
  aiomoqt `03 00 01 08 00` and libquicr's 22 bytes with track extensions, plus
  a PUBLISH_OK case, a REQUEST_OK case, and a non-zero draft 14 case in Rust
  and JS. Each must fail without the fix.

## Closes

- [#4172](https://github.com/moq-dev/moq/issues/4172) - moq-net rejects EXPIRES in SUBSCRIBE_OK
