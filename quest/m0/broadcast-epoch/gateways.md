# [S] Ingest gateways mint an epoch per connection

## Goal

RTMP, SRT, and WHIP ingest publish each incoming connection under a fresh
epoch. An encoder that reconnects to the same stream key becomes a clean
takeover at once. Without it, the reconnect is resumed into the stale
connection's broadcast and stalls viewers until its group sequence catches
up.

## Plan

Mint an epoch per incoming connection and announce it on the broadcast's
route (`moq-rtmp`, `moq-srt`, `moq-rtc`, and the relay wiring); nothing mints
by default. Test a reconnect under the
same key with the stale connection still open. Update `doc/bin/relay/` where it
describes ingest paths.


## Related

- [TS restart](/quest/m0/broadcast-epoch/ts-restart.md) - a signalled restart inside one connection, which this quest's per-connection epoch does not cover
