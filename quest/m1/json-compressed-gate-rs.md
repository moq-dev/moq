# [XS] Rust test for the compressed snapshot gate

## Goal

`rs/moq-json`'s snapshot encoder has a test proving that a delta is admitted
by its encoded size, not its plaintext: a sync-flushed DEFLATE frame can come
out larger than its input, so a plaintext gate lets a patch overflow the
group and evict the snapshot a late joiner needs. The JS encoder gained this
test in the PR that retired `quest/m1/test-flakes.md`.

## Plan

- The gate compares against `moq_net::group::MAX_CACHE_BYTES`, which is too
  large to reach cheaply. Mirror the JS approach: a crate-private budget the
  test can shrink, so it measures real frames and sets a budget between the
  patch's plaintext and encoded sizes.
- Confirm the test fails when the gate measures the plaintext.
