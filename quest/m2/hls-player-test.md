# [M] HLS and DASH browser playback

## Goal

A nightly browser test plays a `moq-hls` broadcast with hls.js and dash.js
and checks that each player reloads, switches renditions, and survives a
mid-run reconfiguration and a long GOP without stalling, with playback time
advancing at real time.

## Plan

From the 2026-10-24 moq-hls audit. Decided with the maintainer on 2026-10-24:
it follows [segment conformance](/quest/m1/hls-conformance.md), reusing its
broadcast fixture, and stays nightly since a browser run is slow. Impair the
path with bufferbloat (a rate limit below the media bitrate behind a deep,
finite queue) to drive a down-switch.

Public API: none. Wire: none.

## Required

- [Segment conformance](/quest/m1/hls-conformance.md) - the fixture and manifest-to-media agreement this builds on
