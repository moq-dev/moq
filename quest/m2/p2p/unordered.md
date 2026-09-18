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
the binding flip; before it, do not build a second reassembly buffer. One
record is one SCTP message, so the unordered writer emits frames for only
one stream per record; mixing streams in a record would stall both on one
lost chunk and is forbidden when unordered is on.

Frames that QUIC itself handles out of order (MAX_DATA, MAX_STREAM_DATA,
RESET_STREAM by final size) need nothing. Params-first setup still holds
because the transport parameters travel in the first record and the receiver
holds later records until it has them.

`RTCDataChannel.ordered` is fixed at `createDataChannel`. The qmux
parameter arrives in the first record on that already-created channel, so
neither binding can learn unordered from the handshake and then flip the
channel. Advertise the capability in the roster `info.json`
(`unordered: true`) before any channel exists, the same place ALPNs live.
The dialer creates the channel with `ordered: false` only when both roster
entries say so, and both sides then send the parameter; otherwise the
channel stays ordered and the parameter is not sent. A channel created
unordered whose peer omits the parameter, or a parameter on an ordered
channel, aborts the session. Do not open a second channel, and do not
learn the mode from the first record. Native iroh and QUIC have no
constructor constraint; the parameter alone is enough there.

Draft: `draft-lcurley-qmux.md` gains the parameter and the relaxed rule; the
data channel row in `draft-lcurley-moq-lite.md` says which mode it runs.
`just drafts check` passes.

Measure with the [harness](/quest/m2/p2p/harness.md) before and after: stall
duration after an induced loss is the number this quest exists to move, and
throughput must not regress.

## Required

- [Harness](/quest/m2/p2p/harness.md) - the loss row this quest is measured against
- [qmux on the QUIC core](/quest/m2/quic/qmux.md) - the receive buffer that makes reassembly free
- [Signaling and policy](/quest/m2/p2p/signal.md) - the roster advertisement that decides ordered before the channel exists
