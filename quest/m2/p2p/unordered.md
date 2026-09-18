# [M] Unordered qmux

## Goal

qmux tolerates reordered records when both sides agree to it, so the data
channel runs `ordered: false` and a lost SCTP chunk stalls only the stream
whose frame it carried. The browser and native transports flip one option;
the WebSocket and TCP bindings are untouched.

## Plan

qmux frames already carry stream offsets; the draft's in-order STREAM
requirement exists so a receiver can deliver without reassembly. Add a
transport parameter, `unordered`, that both sides must send for the relaxed
rule to apply, and receiver-side reassembly by offset for STREAM frames when
it does. Once [qmux on the QUIC core](/quest/m2/quic/qmux.md) lands, that
reassembly is QUIC's own receive buffer and this quest is the parameter plus
the binding flip; before it, do not build a second reassembly buffer.

Frames that QUIC itself handles out of order (MAX_DATA, MAX_STREAM_DATA,
RESET_STREAM by final size) need nothing. Params-first setup still holds
because the transport parameters travel in the first record and the receiver
holds later records until it has them.

Bindings: `@moq/p2p` and the `moq-tokio` transport create the channel with
`ordered: false` when the parameter is negotiated, otherwise as today.
Draft: `draft-lcurley-qmux.md` gains the parameter and the relaxed rule; the
data channel row in `draft-lcurley-moq-lite.md` says which mode it runs.
`just drafts check` passes.

Measure with the [harness](/quest/m2/p2p/harness.md) before and after: stall
duration after an induced loss is the number this quest exists to move, and
throughput must not regress.

## Required

- [Harness](/quest/m2/p2p/harness.md) - the loss row this quest is measured against
- [qmux on the QUIC core](/quest/m2/quic/qmux.md) - the receive buffer that makes reassembly free
