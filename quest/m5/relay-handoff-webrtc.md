# [L] Keep WebRTC flows across a relay handoff

## Goal

An embedder serving WebRTC on the relay's QUIC port (`Relay::webrtc`) keeps
each WebRTC session across a [handoff](/doc/bin/relay/config.md), as QUIC
sessions are kept: the old relay serves its flows until they end or its drain
deadline passes, and new flows reach the new relay. Today the reuseport filter
hashes every non-QUIC datagram onto the newest generation, so a WebRTC
session the old relay holds loses its ICE, DTLS, and RTP at the handoff.

## Plan

Classic BPF keeps no per-flow state, so the filter alone cannot tell the old
relay's flows from new ones. Options:

- The new relay hands a non-QUIC datagram from a source its demux pin table
  does not hold to the old relay over a socket the handoff carries, and the old
  relay answers from its own member, as Meta's Socket Takeover forwards QUIC.
  Recommended: no eBPF loader, and the demux already pins each source.
- An eBPF reuseport program with a map of the sources the old relay pinned.
  Needs `CAP_BPF` and a loader.
- Steer by the ICE username fragment, which only STUN checks carry, not DTLS
  or RTP. Rejected.

## Waiting

- Maintainer: keep this, or drop it until an embedder serves WebRTC on a relay
  that hands off. Recommendation: keep it in m5 with no consumer yet; nothing
  in this repository serves WebRTC with handoff today.
