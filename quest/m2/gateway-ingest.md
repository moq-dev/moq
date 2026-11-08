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

Added 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs and remaining quests): the per-session request shapes settle here too, so m3's rate, report, and wire work adds fields instead of `with_*` setters after the cut.

- `whep::accept` takes a `whep::Request` (`#[non_exhaustive]`), mirroring `whip::Request`, moved here from [rtc-pacing](/quest/m3/rtc-pacing.md), so its rate handle is additive.
- moq-webrtc's `server::Config.udp_bind` becomes `socket: server::Socket` (`#[non_exhaustive]`, only `Bind(SocketAddr)` today), so a fed socket from [one port](/quest/m3/one-port/README.md) is additive.
- moq-srt's dial-out `Client::publish` (`rs/moq-srt/src/dial.rs`), an egress that takes neither `Ingest` nor a request today, takes a `#[non_exhaustive]` `dial::Publish` request (the path, with room for m3's rate `Limit`), so [srt-rate-handle](/quest/m3/srt-rate-handle.md) reaches egress additively.
- Per-session knobs m3 adds (the rate `Limit`, report and echo, the SRT `Wire`) are fields on these request structs or on `Ingest`, never new setters on `Client` or `Publish`.
