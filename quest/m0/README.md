# m0: bug fixes

## Goal

Defects in what main or dev ships today: crashes, races, protocol violations,
wrong output, security gaps, regressions an unreleased change introduced, and
the missing tests that let them through.

## Plan

Fix where the defect is, which is usually main; a quest says so when it is dev.
Security and credential exposure lead; user-visible breakage
next; hardening, tooling, and test debt close the list. Each fix lands with a
regression test per Root Cause First.

## Quests

- [Play tune-in backpressure](/quest/m0/play-tunein-backpressure.md) - moq play: a tune-in burst larger than the video queue parks the decoder, so the clock never reaches live at a wide `--delay`
- [Play audio rendition gap](/quest/m0/play-audio-rendition-gap.md) - moq play: a retired audio rendition drains its sink before the replacement fills one, so the switch costs a `--delay` of silence
- [Auto latency](/quest/m0/3477-watch-auto-latency.md) - js/watch: the measured auto latency (#3517 on dev) is proven clean in a browser and against the public relay
- [Jitter estimator](/quest/m0/3479-mux-jitter-flush-span.md) - moq-mux: catalog jitter is the publisher's maximum flush span, never a running minimum
- [WebKit gate](/quest/m0/webkit-webtransport-gate.md) - js/net: every WebKit engine takes the WebSocket path, not just the Safari brand, so iOS Chrome and Firefox stop freezing after two minutes
- [Publisher priority](/quest/m0/3534-ietf-publisher-priority.md) - moq-net: a track's publisher priority survives a moq-transport hop instead of being flattened to 0
- [TRACK_STATUS refusal](/quest/m0/3492-ietf-track-status-refusal.md) - moq-net: TRACK_STATUS gets a NOT_SUPPORTED refusal instead of a silent drop
- [Connect auth race](/quest/m0/3532-connect-auth-race.md) - moq-native: a 403 on the WebSocket arm no longer fails a connect whose QUIC arm is still in flight; moq-ffi can disable the fallback
- [Resume info](/quest/m0/resume-info-newest.md) - moq-net: resume reports segment zero's track info, so a replaced broadcast rescales timestamps on the predecessor's timescale
- [TS restart stall](/quest/m0/3533-ts-export-restart-stall.md) - moq export ts: a content restart on a continuous timeline no longer fences video and primary audio for good
- [Failure artifacts](/quest/m0/qa-failure-artifacts.md) - a failing harness run keeps its run directory and a Playwright trace, and CI uploads them
- [Harness drive-bys](/quest/m0/harness-drive-bys.md) - decide whether the relay listening kind field that came with #3509 stays
- [Capture denial](/quest/m0/browser-permission-qa.md) - moq-publish swallows a refused camera or microphone and retries forever; it surfaces the denial and recovers on grant
- [Publisher audio unlock](/quest/m0/publish-audio-unlock.md) - the publisher's capture AudioContext stays suspended when the page had no gesture, so no audio is ever encoded
- [Impaired path](/quest/m0/transport-impairment-profile.md) - the transport drills run over a seeded, impaired UDP path on any host
- [Tooling](/quest/m0/tooling/README.md) - justfiles become a one-line menu over `sh/`, one impact map scopes CI, and every workflow step runs a recipe
