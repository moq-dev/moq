# [S] moq-relay's tests pass without default features

## Goal

`just rs test -p moq-relay --no-default-features` passes: every test that
needs QUIC is gated on the `quic` feature, and the nightly feature matrix runs
that configuration so it stays green.

## Plan

Found by [moq-dev/moq#8081](https://github.com/moq-dev/moq/pull/8081): on its
merged head the configuration had 29 failures, all QUIC-only tests that don't
check for the feature. No CI row ran it, so it rotted unseen.
