# [M] Carrier-voice verdict

## Goal

A measured go/no-go verdict decides whether MoQ earns a maintained role in
programmable carrier voice, and if so whether that role is developer-facing,
an internal gateway fabric, or both. An inconclusive result creates no product
commitment.

## Plan

- Replay the same Opus source through direct RTP, the MoQ lab path, and
  [RTP over QUIC](https://datatracker.ietf.org/doc/draft-ietf-avtcore-rtp-over-quic/)
  (RoQ). Its current Internet-Draft is expired prior art, not an assumed
  adoption target: use a credible implementation if one remains runnable;
  otherwise record why that arm cannot support a product decision instead of
  building a second production stack just for the comparison.
- Use fixed clean, constrained-cellular, random-loss, burst-loss, and simulated
  Wi-Fi/cellular path-change profiles. Record call setup time, one-way audio
  latency and jitter, late/lost frames, recovery gaps, wire overhead, gateway
  CPU, and connection continuity. Keep codec, packetization, source audio,
  network trace, and hardware fixed across arms.
- Measure the proposed advantage directly: attach the auxiliary subscriber and
  report its incremental setup work, latency, bandwidth, and gateway CPU. Also
  document the equivalent RTP media-fork implementation so "programmable" is
  compared against a real alternative rather than asserted.
- A go requires the full call lifecycle and mobility profiles to complete
  without a protocol workaround, and MoQ's p99 one-way latency to stay within
  50 ms of direct RTP while the auxiliary subscriber remains an ordinary
  authorized subscription. Treat the threshold as a rejection bound, not a
  claim that a 50 ms regression is desirable.
- The verdict separately answers: whether in-band MoQ call control is simpler
  than HTTP plus media streams, whether QUIC path migration helps in the
  implementations we can actually ship, whether object overhead is acceptable
  for 20 ms audio, and whether gateway-to-gateway transport adds value beyond
  the developer-facing API. Recommend only the smallest surface supported by
  the evidence and create implementation quests only for that surface.
- The conventional inbound SIP gateway product is moq.pro (downstream) work
  and remains the standing telephone path regardless of this verdict.

## Required

- [Developer-to-SIP proof](/quest/m3/carrier-voice/proof.md) - provides the
  working MoQ path and reproducible harness
