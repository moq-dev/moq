# [S] Moxygen announce-subscribe works through moq-relay

## Goal

moq-dev/smoke's minimal Moxygen interop client passes `announce-subscribe`
through moq-relay on each transport and draft the lane claims to cover,
and that failure no longer hides behind `continue-on-error`.

## Plan

The [2026-10-15 Moxygen job](https://github.com/moq-dev/smoke/actions/runs/38739639896/job/116437673088)
reports success while its script fails. Both WebTransport and raw QUIC
negotiate draft-16, then TAP case 4 (`announce-subscribe`) fails with
`Subscribe failed: Session closed`. The other five cases pass. The workflow
explicitly marks the step `continue-on-error` for this known mismatch.

Reproduce the minimal lane and trace the failed request before choosing a
protocol or routing change. Fix a first-party defect in moq-dev/moq with a
regression at its source, then verify smoke's Git-head lane. An upstream
client defect needs a reduced reproducer and a clearly attributed result;
posting outside moq-dev and kixelated requires maintainer approval.

Remove the exception once the full claimed lane passes. Do not accept a
session close as successful media exchange or hide the failing case. Any
wire change keeps published drafts compatible and updates Rust, JS, and
the matching draft together.

This minimal six-case lane is independent of the larger moq-test
passthrough conformance work. The maintainer requested fixes or quests for
the investigated failures on 2026-10-15.

## Related

- [moq-transport passthrough](/quest/m1/ietf-extension/README.md) - broader object forwarding and moq-test conformance
