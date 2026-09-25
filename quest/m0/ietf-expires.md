# [S] Accept EXPIRES from IETF publishers

## Goal

A moq-net subscriber accepts the EXPIRES parameter (0x08) that aiomoqt and
libquicr send in SUBSCRIBE_OK, so a relay can subscribe upstream to either of
them. PUBLISH_OK and REQUEST_OK accept it too. What the publisher sends is
unchanged.

## Plan

- `SubscribeOk`, `PublishOk`, and `RequestOk` in `rs/moq-net/src/ietf/`
  decode 0x08 on every draft that uses parameters. Today `decode_params!`
  rejects unlisted keys with `InvalidValue`.
- A non-zero EXPIRES in SUBSCRIBE_OK becomes the track's `Info::max_age`, in
  milliseconds, in place of `origin.default_max_age()`. The two differ: EXPIRES
  is a wall-clock subscription lifetime and max_age is a media-time retention
  window. EXPIRES is loosely defined, though, since it can end a subscription
  in the middle of a group, and max_age is the closest thing moq-net has. Zero
  or absent keeps the default.
- Draft 14 carries expires as a fixed SUBSCRIBE_OK field and currently returns
  `Unsupported` when it is non-zero. Give it the same mapping and drop that
  test.
- PUBLISH_OK and REQUEST_OK parse the value and drop it, because there is no
  incoming track to apply it to.
- Regression tests: decode the two captured SUBSCRIBE_OK bodies from #4172,
  aiomoqt `03 00 01 08 00` and libquicr's 22 bytes with track extensions, plus
  a PUBLISH_OK case and a REQUEST_OK case. Each must fail without the fix.
- JS already accepts EXPIRES everywhere (`js/net/src/ietf/parameters.ts`), so
  this is Rust-only.

## Closes

- [#4172](https://github.com/moq-dev/moq/issues/4172) - moq-net rejects EXPIRES in SUBSCRIBE_OK

## Related

- [Max age over EXPIRES](/quest/m1/ietf-max-age.md) - the follow-up that makes max_age default to 0 and sends it as EXPIRES
