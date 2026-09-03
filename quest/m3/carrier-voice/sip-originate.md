# [M] SIP call origination

## Goal

The `moq-sip` stack can originate one audio-only SIP call and expose the
resulting dialog as Opus frames in and out. The first consumer is the
programmable carrier-voice lab, not a production outbound-calling product.

## Plan

- Extend the selected SIP stack rather than building a second SIP adapter. The
  embedder supplies the request URI, asserted line identity, credentials, and
  Opus source/sink; the crate owns INVITE transactions, SDP offer/answer,
  provisional and final responses, RTP/SRTP, cancellation, and BYE.
- Reuse the inbound stack's Opus/G.711 negotiation, transcoding, RTP clock
  normalization, and teardown. Exercise ringing, answer, rejection, caller
  cancellation, remote hangup, and timeout against a real softphone or test
  PBX.
- Keep PSTN routing, number ownership, caller-ID policy, emergency calling,
  registrar support, DTMF, billing, and production trunk credentials outside
  this quest. The lab gateway decides whether an authenticated caller may use
  a line before it asks `moq-sip` to originate.

## Required

- [SIP media stack](/quest/m3/sip-stack.md) - origination extends the same
  dialog, codec, and RTP implementation

## Related

- [Developer-to-SIP proof](/quest/m3/carrier-voice/proof.md) - the first
  end-to-end consumer
