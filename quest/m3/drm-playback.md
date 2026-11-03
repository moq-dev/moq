# [L] Protected playback in the browser

## Goal

`<moq-watch>` plays protected CMAF renditions through MSE and EME, using the hang catalog's `drm` section for the key system and license server, instead of hiding them.
Start once a consumer asks.

## Plan

Decided with the maintainer on 2026-11-03: plan browser playback, deferred until there is demand, and no native playback.
Native players have no CDM; only ClearKey would work without platform APIs, and no commercial DRM uses it.

WebCodecs cannot decrypt, and EME only works on a media element fed through MSE.
Watch removed its MSE backend in [#2288](https://github.com/moq-dev/moq/pull/2288), so this brings back a second pipeline only for protected renditions.

Open:
- how it shares latency control with the WebCodecs path;
- Safari's ManagedMediaSource with FairPlay's certificate URL.

## Required

- [Protected CMAF import](/quest/m2/drm/import.md) - the hang `drm` section this reads, and the renditions it stops hiding
