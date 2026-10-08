# [S] JS createTrack takes over a queued request

## Goal

In `@moq/net`, `createTrack` and `insertTrack` on a name with a queued or
pending request answer that request and continue the name's group and
datagram sequences, as Rust's `create_track` does, instead of throwing
`duplicate track`. The per-name sequence map stays bounded without forgetting
sequences that were written within the broadcast.

## Plan

Decided 2026-10-06 while settling #4929: that PR coalesces JS publishing-side
subscriptions per name and accepts the `duplicate track` throw for now; this
quest brings JS to parity. Drop unused entries only when no track holds them
and they never saw a write. Written sequence state survives until the
broadcast ends: evicting it and restarting under the same name would reuse
content identities and stall resumed readers.

Bound admission of additional names if retaining their sequence state would
exceed the map's bound; settle the refusal surface during implementation.
Starting sequences over requires a new broadcast. Test queued-request
takeover, explicit sequence advancement, replacement continuity, and bounded
name admission. Update the JS net docs for the behavior change.

Public API: behavior change in `@moq/net` only. Wire: none.
