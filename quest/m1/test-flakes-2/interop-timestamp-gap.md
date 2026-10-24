# [S] Interop audio has no timestamp gap under load

## Goal

`rust -> js-native-node` (and `rust-swapped`) never fail the audio check with
a timestamp gap under load.

## Plan

Seen in the interop runs of
[moq-dev/moq#6447](https://github.com/moq-dev/moq/pull/6447),
[moq-dev/moq#6530](https://github.com/moq-dev/moq/pull/6530), and
[moq-dev/moq#6517](https://github.com/moq-dev/moq/pull/6517): a gap up to the
next 1 s group, passing on rerun. Reproduce under CPU load, find whether the
publisher, relay, or the Node subscriber drops the audio, and fix that layer.

Public API: none. Wire: none.
