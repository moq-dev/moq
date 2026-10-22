# [M] Broadcast egress library

## Goal

A UI-free library subscribes once to a MoQ broadcast and remuxes it to any
number of RTMP, RTMPS, and WHIP destinations. It never decodes or encodes.
- **Destinations:** the caller adds and removes them at runtime, each with
  live status.
- **Failure:** a dropped destination redials without disturbing the others.
- **Consumers:** the studio's destinations use it locally, and a hosted
  service can run the same code server-side.

## Plan

Decided 2026-10-22 with the maintainer:
- **A library, not a gateway:** this repository ships no gateway service
  and no CLI mode for it.
- **Track selection:** each destination names the renditions it takes. The
  publisher already produced every codec a destination needs, so there is
  nothing to transcode.
- **Name:** settle the crate's name in this quest with the maintainer.
- **RTMPS:** moq-rtmp dials `rtmps://` natively and keeps the URL-derived
  `tcUrl` that plaintext `Client::connect` advertises. Today the TLS route,
  `Client::with_stream`, sends no `tcUrl`, and several ingest servers reject
  that.
- **Untrusted destination URLs:** a server-side host must be able to refuse
  private and loopback addresses, so dialing goes through a caller-supplied
  policy rather than resolving any URL it is handed.
- **Benchmark:** sweep concurrent broadcasts against destinations per
  broadcast, so any cost that grows with the whole table, rather than the
  touched destination, shows up as a slope.

## Required

- [The studio app](/quest/m3/studio/app.md) - the published broadcast this consumes
