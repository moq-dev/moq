# [S] The publish support panel lists only codecs publishing accepts

## Goal

The `<moq-publish-support>` settings panel (`js/publish/src/support/`)
decides video encode support with the same encoder setup probe the publisher
uses, including the Annex B output check. On Safari it stops listing HEVC as
supported when publishing would skip it. Done when the panel and the
publisher share one probe, and a test with an encoder that emits
length-prefixed output shows the panel reporting that codec unsupported.

## Plan

Planned 2026-11-03 (maintainer, `/quest-plan`), from the follow-ups of
[moq-dev/moq#7765](https://github.com/moq-dev/moq/pull/7765), which makes
the publisher refuse an encoder whose output ignores the requested Annex B
format (`probeEncoder` in `js/publish/src/video/encoder.ts`). Safari's HEVC
encoder does that. The panel's `probe` (`js/publish/src/support/video.ts`)
still trusts `VideoEncoder.isConfigSupported` alone.

- Start once #7765 has merged, since its probe is what this reuses.
- Share one probe rather than copy it. Move it somewhere both modules
  import, and keep the panel's software/hardware split, which runs a probe
  per acceleration preference.
- The output check encodes a real frame, which costs more than
  `isConfigSupported`. Cache the result per codec and preference if the
  panel probes repeatedly.

Public API: none. The panel's output changes on browsers whose encoder
ignores Annex B. Wire: none.
