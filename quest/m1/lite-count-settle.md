# [S] lite-07 subscribers settle on the stream count

## Goal

On moq-lite-07, Rust and JS subscribers stop waiting for a subscription's tail
once they have read the headers of as many group streams as SUBSCRIBE_END
counts, so a group the publisher skipped or never opened costs no grace. A
late stream below the end is still accepted within the grace, which stays for
a stream reset before its header arrived. lite-05 and -06 keep the DROP
accounting JS already has and Rust track tail adds.

## Plan

- Both publishers already send the count, and both subscribers decode it
  (`lite::SubscribeEnd::streams` in Rust, `SubscribeEnd.streams` in JS) and
  ignore it.
- JS track tail has landed, and its `Tail` already counts streams. Rust track
  tail has landed (`rs/moq-net/src/tail.rs`), so lite-07's completion check
  becomes "headers read >= Stream Count" after SUBSCRIBE_END, in place of
  every sequence from start to end being covered. The Rust subscriber needs
  the same count.
- Tests in both languages: a late stream after SUBSCRIBE_END, a skipped group
  that settles without the grace, a reset stream, and a count of zero. Add
  the Rust-JS case to the track tail interop test.

## Related

- [Track tail interop](/quest/m1/track-tail-interop.md) - the Rust-JS case this adds its count check to
- [Reliable stream reset](/quest/m1/quic/reliable-reset.md) - makes the count exact by keeping a reset stream's header
