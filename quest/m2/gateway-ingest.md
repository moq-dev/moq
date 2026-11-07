# [M] Gateways share one ingest config and one rate cap

## Goal

Every protocol gateway configures ingest the same way, and no setter applies
to a direction it cannot affect:

- `moq_mux::Ingest { max_age, bandwidth, linger }` (`#[non_exhaustive]`,
  `Default`) replaces the 18 `with_max_age`/`with_bandwidth`/`with_linger`
  setters and fields on moq-rtmp's and moq-srt's `Client` and `Publish`, and
  moq-webrtc's `server::Config` and `client::Config`. It is taken by
  `rtmp::Publish::accept`, `rtmp::Client::pull`, `srt::Publish::accept`,
  `srt::Client::pull`, `moq_webrtc::whip::Request`, and
  `moq_rtsp::Client::pull`. Egress keeps its own setters on `publish`.
- `moq_webrtc::rate::{Cap, Limit}` move to `moq_net::bandwidth` beside
  `Rate`, so WHIP, WHEP, SRT, and the relay's grant caps share one live cap
  handle.

## Plan

Found by the 2026-11-07 API shape audit (apps). Today a bidirectional client
documents its ingest setters as "pull direction only", so setting
`with_linger` and then calling `publish` is silently ignored. moq-rtsp
publishes with fixed defaults and gains the same knobs. Land before the cut
so moq-webrtc never ships its own `Cap`/`Limit` and moq-srt never copies
them: [srt-rate-handle](/quest/m3/srt-rate-handle.md) and
[rtc-pacing](/quest/m3/rtc-pacing.md) take `bandwidth::Limit`. moq-cli's
`import --max-age`/`--linger` build one `Ingest`. Whether
`moq_quic::limits::Rate`'s `Unlimited` variant can become `Option<Rate>` is
out of scope.
