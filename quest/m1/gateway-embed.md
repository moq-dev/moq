# [M] The gateway crates expose the loop their binaries run

## Goal

An in-process embedder of `moq-hls`, `moq-rtmp`, or `moq-rtc` calls the
crate's accept loop and request handler with its own origin handle instead
of copying them. moq.pro's `rs/edge/src/{hls.rs, rtmp/mod.rs}` carry the
copies: the HLS route parser, bearer extraction, broadcaster pool, and the
playable/init/media-playlist dance; the RTMP accept loop with TLS sniffing
and its `ActivePaths` dedup.

## Plan

Additive, so on main:

- `moq_hls::Server::new(config)` with `router(origin)` for the convenience
  path and `broadcaster(&self, source: moq_mux::Source)` keyed by the
  caller's per-scope consumer; `server::Route` and `parse_route` public;
  `Broadcaster::{closed, is_closed}` public and ungated;
  `Rendition::playlist(query)` folding the three-step playable/init/media
  dance; `master::{VideoVariant, AudioVariant, render}` public with a `uri`
  per variant (the recorder forked 160 lines of `export/master.rs` for a
  different layout). [HLS generation](/quest/m1/hls-generation.md) lands on
  whatever layout hook this becomes, for the DASH renderer
  (`Broadcaster::manifest`, `export/mpd.rs`) as well.
- `moq_rtmp::listen::Config.tls: Option<Arc<ServerConfig>>` meaning sniff
  and serve both on one port, and `ActivePaths` public or folded into
  `Publish::accept`.
- `moq-rtc` gains a `server` feature (default on) gating axum, the routers,
  and `pub use axum`; moq.pro's `default-features = false` is a no-op today.

Public API: additive. Wire: none.

## Related

- [HLS generation](/quest/m1/hls-generation.md) - the URL hook that depends on the master layout
