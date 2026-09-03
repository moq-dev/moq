# [L] SIP media stack

## Goal

A `moq-sip` crate that terminates one inbound audio call leg: INVITE-only SIP
over UDP/TCP/TLS, SDP offer/answer for Opus and G.711 (SRTP when offered), and
a plain RTP media leg exposed to the embedder as Opus frames in and out.
Registrar, outbound, video, and DTMF are out of scope; REGISTER gets 405 and
RFC 4733 telephone-events are ignored. This quest completes when the crate is
consumable by an embedder.

## Plan

- Open with an evaluation, spike-backed like the
  [WebRTC bridge verdict](/quest/m3/livekit-webrtc-bridge.md): drive a real
  inbound call through the candidate full Rust SIP stacks (ezk-sip, rvoip,
  and whatever else is current) and adopt the one that holds up. Fallback if
  none do: an existing parser crate (e.g. rsip) for message/SDP syntax plus a
  minimal own INVITE-only transaction and dialog engine - the surface is
  small once registrar, proxy, and outbound are out.
- The media leg is plain negotiated RTP: `moq-rtc`/str0m is ICE/DTLS-first
  and its reusable session internals are crate-private, so it is precedent,
  not a base. Consider publicizing moq-rtc's codec bridges rather than
  duplicating the RTP-to-hang mapping, and reuse `moq-audio`'s Opus and
  resampler for the G.711<->Opus transcode (mono 8 kHz; G.711 companding is
  new code, nothing in the repository has it).
- RTP wall-clock normalization off RTCP sender reports, as moq-rtc does.
- The embedder decides paths and auth; the crate's API is
  "answer this INVITE, give me the caller as Opus, take Opus to play" plus
  call teardown. Silence generation until playback audio exists lives here,
  so every embedder gets answer-with-silence for free.
- The SIP edge gateway and line provisioning that consume this crate as an
  inbound-call product are moq.pro (downstream) work.
