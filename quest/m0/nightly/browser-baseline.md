# [M] The browser backend baseline completes on IETF22

## Goal

The WASM and UniFFI backend baseline completes every supported transport case without an internal error.

## Plan

[The browser baseline job](https://github.com/moq-dev/moq/actions/runs/40698329026/job/122851001597) fails under
`just test wasm-baseline --uniffi` with `evaluate: MoqError.Internal` during
the moq-transport-22 benchmark. Its relay log also contains a uni-stream
`not found` error; that is evidence to investigate, not an established cause.
The retained `wasm-backend-baseline` artifact contains the run's diagnostics.

Reproduce the failing cell on current main and trace the original transport
or model error through the bindings. Preserve completion and frame-integrity
checks. Review the already-landed reset/error mapping fixes before attributing
this to UniFFI itself.

Public API: none expected. Wire: none expected.
