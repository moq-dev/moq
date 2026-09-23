# [S] Ingest gateways mint an epoch per connection

## Goal

RTMP, SRT, and WHIP ingest publish each incoming connection under a fresh
epoch. An encoder that reconnects to the same stream key becomes a clean
takeover at once, instead of a second publisher racing the stale one until it
times out.

## Plan

Use the origin default where the gateways create broadcasts (`moq-rtmp`,
`moq-srt`, `moq-rtc`, and the relay wiring). Test a reconnect under the same
key with the stale connection still open. Update `doc/bin/relay/` where it
describes ingest paths.

## Required

- [Origin](/quest/m1/broadcast-epoch/origin.md) - the publish default
