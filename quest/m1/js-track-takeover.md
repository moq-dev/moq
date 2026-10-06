# [S] JS createTrack takes over a queued request

## Goal

In `@moq/net`, `createTrack` and `insertTrack` on a name with a queued or
pending request answer that request and continue the name's group and
datagram sequences, as Rust's `create_track` does, instead of throwing
`duplicate track`. The per-name sequence map stays bounded.

## Plan

Decided 2026-10-06 while settling #4929: that PR coalesces JS publishing-side
subscriptions per name and accepts the `duplicate track` throw for now; this
quest brings JS to parity. The per-name sequence map only grows on accepted
requests and is never pruned; bound it the way #4929 bounds Rust's (drop
names that never wrote and that no track holds).

Public API: behavior change in `@moq/net` only.
