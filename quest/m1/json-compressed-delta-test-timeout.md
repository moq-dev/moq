# [S] Make the compressed-delta cache regression reliable under load

## Goal

The encoded-size regression in `js/json/src/snapshot/snapshot.test.ts` completes
within the existing test budget on a busy developer machine.

## Plan

While validating #3448, `a compressed delta is gated on its encoded size, not its
plaintext` exceeded Bun's five-second timeout twice in `just test default
origin/dev` with Bun 1.3.13. An isolated run with host Bun 1.2.23 also timed out.
CI passed on the same source. The machine was concurrently compiling Rust;
contention is a hypothesis, not a confirmed diagnosis.

Trace the producer completion and compression work before changing the test.
Preserve the assertion that encoded frames cannot evict the group's snapshot,
and verify the test still fails if the encoded-size guard is removed. Prefer a
smaller deterministic fixture or explicit completion over raising the timeout.
