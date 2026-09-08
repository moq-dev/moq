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

- [Auto latency](/quest/m0/3477-watch-auto-latency.md) - js/watch: auto latency follows measured arrivals, and the audio ring holds slack and re-buffers
- [Jitter estimator](/quest/m0/3479-mux-jitter-flush-span.md) - moq-mux: catalog jitter is the publisher's maximum flush span, never a running minimum
- [Encoder lag](/quest/m0/publish-audio-lag-measure.md) - js/publish: measure the audio encoder's input-to-output lag on a high-RTT path
- [IETF error codes](/quest/m0/ietf-error-codes.md) - every code on a moq-transport wire is a registered value for the negotiated draft, requests and stream resets alike
- [Resume info](/quest/m0/resume-info-newest.md) - moq-net: resume reports segment zero's track info, so a replaced broadcast rescales timestamps on the predecessor's timescale
- [TS timebase discontinuity](/quest/m0/ts-forward-discontinuity.md) - preserve source-signalled clock changes through import and export
- [Group charge](/quest/m0/group-charge.md) - charge real per-group cost so MOQ_CACHE_CAPACITY bounds real memory
- [uring all-features](/quest/m0/uring-all-features-build.md) - moq-uring does not compile with `--all-features`, so the nightly features gate fails on it
- [PR behavioral gates](/quest/m0/pr-behavioral-gates.md) - run the applicable interop and platform gates on source PRs before merge
- [Failure artifacts](/quest/m0/qa-failure-artifacts.md) - retain inspectable traces, logs, and rerun commands when QA fails
- [Browser permission QA](/quest/m0/browser-permission-qa.md) - nothing covers what the publisher does when the user denies the camera or microphone
- [Publisher audio unlock](/quest/m0/publish-audio-unlock.md) - the publisher's capture AudioContext stays suspended when the page had no gesture, so no audio is ever encoded
- [Media QA on other engines](/quest/m0/browser-media-qa-engines.md) - the media output checks only run on the pinned Chromium
- [Teardown error classification](/quest/m0/js-teardown-error-classification.md) - a browser consumer cannot tell a requested end from a fault, so QA cannot be strict during one
- [Impaired-path profile](/quest/m0/transport-impairment-profile.md) - run the transport failure drills over a delayed, lossy, rate-limited QUIC path
