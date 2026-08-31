# [S] Archive catalog cutover

## Goal

The root `timeline` entry is removed after every supported publisher can
advertise a stored `archive`, with no intermediate live-HLS outage.

## Plan

Delete the legacy `timeline` catalog entry and its consumer fallback; `archive`
becomes the only HLS availability contract. Do not retain an alias or
dual-write after the migration.

Cut the release only after `moq-cli` and browser publishing attach their
archive writers. Downstream (moq.pro) deployments should adopt that release
only once their RTMP, SRT, and WHIP gateways attach the same writer in one
change, so deployed native paths never lose HLS between the schema change and
store enrollment.

## Required

- [Recording writer](/quest/m1/archive/writer.md) - wires every `moq-cli` import path
- [Browser archive](/quest/m1/archive/browser.md) - closes the remaining browser publish path
