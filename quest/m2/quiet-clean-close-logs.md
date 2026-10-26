# [XS] Quiet logs for clean ends

## Goal

A probe stream that ends cleanly, or a track request that is unroutable, logs
at debug instead of warn; other errors stay at warn.

## Plan

- `probe stream error` in `rs/moq-net/src/lite/publisher.rs` and
  `subscriber.rs` warns even for `no error`; `track info failed` in
  `subscriber.rs` warns for Unroutable; also check the SRT server's
  unroutable line. From [moq-dev/moq#6632](https://github.com/moq-dev/moq/pull/6632).
