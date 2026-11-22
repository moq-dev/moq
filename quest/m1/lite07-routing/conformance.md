# [S] Rust and JS lite-07 match the draft on cost ceilings, zero link cost, and pipelined announce requests

## Goal

Rust caps lite-07 claim costs at 2^62-1 as the draft says, JS refuses a zero
link cost in SETUP, JS checks an ANNOUNCE_REQUEST against the prefixes it
actually holds, and `decode_forwarded` fails instead of returning a `None`
no caller can use.

## Plan

Found 2026-11-21 by the main → release routing audit; none of these is
tracked elsewhere, and none gates the cut.

1. **Cost ceiling.** Rust saturates claim costs at `u64::MAX`
   (`rs/moq-net/src/route/relay.rs:332,374`) and `encode_claim` writes them
   raw (`lite/announce.rs:233`). The draft says costs saturate at 2^62-1
   (about line 1365), and JS clamps on decode, so a peer that declares a huge
   SETUP cost has over-ceiling costs forwarded by Rust.
2. **Zero link cost in JS.** `js/net/src/lite/setup.ts` never reads
   PARAM_COST (0x4), so a lite-07 cost of 0 is accepted; the draft makes it a
   PROTOCOL_VIOLATION (about line 1075), and Rust refuses it
   (`lite/setup.rs:343-345`). Keep `cost + 1n` in `routed.ts:318`: the draft
   lets a receiver charge a local value of at least 1, and a browser
   forwards nothing. Fix its comment, which claims to match Rust's default.
3. **Pipelined requests in JS.** `answer.ts:177-180` clears `#request` before
   `#edit` finishes its pre-OK `#sync()` (`:198`), so a peer that sends its
   next edit before ANNOUNCE_OK, which the draft forbids, gets past the guard
   at `:102` and is checked against stale `#prefixes`. A scratch test nested
   `a/x` under a pending `a`. Rust holds the request until the edit is
   answered (`rs/moq-net/src/lite/publisher/routed.rs:263-265`). Only the
   misbehaving peer's own session is affected; clear the request just
   before sending the OK.
4. **`decode_forwarded`.** It returns `Option` and its docs say a bad
   instance "withdraws only the claim" (`lite/instance.rs:93-95`,
   `:180-195`), but its one caller (`lite/announce.rs:611`) correctly turns
   `None` into a PROTOCOL_VIOLATION, as the draft requires (about line 1350).
   Return the value or fail, and fix the doc and test.

Public API: none. Wire: Rust conforms to the draft's cost ceiling.
