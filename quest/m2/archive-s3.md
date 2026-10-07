# [S] Archive S3 wire proof

## Goal

The `moq-archive` proof (`rs/moq-archive/src/proof.rs`) also runs through
`object_store`'s S3 client against an in-process S3-compatible server, in CI
with no external network.

## Plan

The proof covers memory, local disk, and an S3-style listing fake
(`rs/moq-archive/src/mock.rs`), but not the S3 client: percent-encoded keys
such as `catalog%2Ejson` inside request URLs, `PutMode::Create` through
`If-None-Match`, `start-after` listing, and continuation tokens. Evaluate a
maintained in-process server (for example `s3s` with `s3s-fs`) on loopback;
refuse one that ignores conditional creates, since collisions would pass
silently. Gate it behind a dev-dependency feature if the build cost is large,
and wire it into at least the nightly workflow.

## Related

- [Timeline-indexed MoQ archives](/quest/m1/archive/README.md)
