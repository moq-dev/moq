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

- [#2405](/quest/m0/2405-js-net-connect-logs-on-every-connection-at-the-wrong.md) - js/net: connect logs print the JWT in the relay URL
- [#3360](/quest/m0/3360-js-watch-broadcast-is-undefined-at-initialization.md) - js/watch: a framework binding the element reads `broadcast` before the custom element is upgraded
- [Adapter namespace map](/quest/m0/rs-adapter-namespace-map.md) - moq-net: a duplicate PUBLISH_NAMESPACE on draft-14/15 strands the first request, and the map never shrinks
- [IETF error codes](/quest/m0/ietf-error-codes.md) - every code on a moq-transport wire is a registered value for the negotiated draft, requests and stream resets alike
- [Resume info](/quest/m0/resume-info-newest.md) - moq-net: resume reports segment zero's track info, so a replaced broadcast rescales timestamps on the predecessor's timescale
- [#3080](/quest/m0/3080-fix-watch-audio-ring-truncate-can-race-the-worklet-reader.md) - watch: an audio ring truncate can race the worklet reader for one quantum
- [#3363](/quest/m0/3363-js-watch-a-broadcast-republished-on-one-session-keeps-resuming.md) - js/watch: a broadcast republished under its name on one session keeps resuming
- [#3361](/quest/m0/3361-js-every-moq-package-a-package-imports-is-declared.md) - js: every @moq package a package imports is a declared dependency
- [#2833](/quest/m0/2833-moq-export-ts-a-rewound-timeline-stalls-the-si-table.md) - moq export ts: a rewound timeline stalls SI tables, PCR, and pacing until the media clock catches up
- [#3326](/quest/m0/3326-moq-audio-held-resampler-frames-keep-their-source-timestamp.md) - moq-audio: held resampler frames keep their source timestamp
- [Group charge](/quest/m0/group-charge.md) - charge real per-group cost so MOQ_CACHE_CAPACITY bounds real memory
- [Cache governor lifetime](/quest/m0/cache-governor-lifetime.md) - stop the headroom task after setup failure or the last owner drops
- [uring all-features](/quest/m0/uring-all-features-build.md) - moq-uring does not compile with `--all-features`, so the nightly features gate fails on it
- [Go smoke client](/quest/m0/smoke-go-client.md) - the interop matrix has no Go client, so nothing in CI exercises the Go wrapper
