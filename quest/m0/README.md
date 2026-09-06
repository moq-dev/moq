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

- [#3360](/quest/m0/3360-js-watch-broadcast-is-undefined-at-initialization.md) - js/watch: a framework binding the element reads `broadcast` before the custom element is upgraded
- [Adapter namespace map](/quest/m0/rs-adapter-namespace-map.md) - moq-net: a duplicate PUBLISH_NAMESPACE on draft-14/15 strands the first request, and the map never shrinks
- [Playout clock](/quest/m0/playout-clock.md) - moq play presents on a clock it controls, with a `--delay` offset and forward re-anchoring
- [IETF error codes](/quest/m0/ietf-error-codes.md) - every request error a moq-transport wire carries is a registered value for the negotiated draft, in Rust and js/net
- [Resume info](/quest/m0/resume-info-newest.md) - moq-net: resume reports segment zero's track info, so a replaced broadcast rescales timestamps on the predecessor's timescale
- [#3363](/quest/m0/3363-js-watch-a-broadcast-republished-on-one-session-keeps-resuming.md) - js/watch: a broadcast republished under its name on one session keeps resuming
- [#3361](/quest/m0/3361-js-every-moq-package-a-package-imports-is-declared.md) - js: every @moq package a package imports is a declared dependency
- [JavaScript rewind cursor](/quest/m0/js-rewind-cursor.md) - detect a rewind in the unread active group
- [SRT rewind pacing](/quest/m0/srt-rewind-pacing.md) - re-anchor SRT egress timestamps when the media timeline restarts
- [TS timebase discontinuity](/quest/m0/ts-forward-discontinuity.md) - preserve source-signalled clock changes through import and export
- [#3326](/quest/m0/3326-moq-audio-held-resampler-frames-keep-their-source-timestamp.md) - moq-audio: held resampler frames keep their source timestamp
- [Group charge](/quest/m0/group-charge.md) - charge real per-group cost so MOQ_CACHE_CAPACITY bounds real memory
- [Cache governor lifetime](/quest/m0/cache-governor-lifetime.md) - stop the headroom task after setup failure or the last owner drops
- [uring all-features](/quest/m0/uring-all-features-build.md) - moq-uring does not compile with `--all-features`, so the nightly features gate fails on it
- [Echo-delay test runtime](/quest/m0/aec-test-runtime.md) - keep the audio regression within the normal workspace test budget
- [Go smoke client](/quest/m0/smoke-go-client.md) - the interop matrix has no Go client, so nothing in CI exercises the Go wrapper
- [Auth expiry test](/quest/m0/auth-expiry-test-clock.md) - remove mixed-clock scheduling from the credential expiry regression
- [Retirement race](/quest/m0/transcode-retirement-race.md) - moq-transcode: retirement has no coverage for a fetch that is still opening its decoder
- [Quest ready gate](/quest/m0/quest-ready-gate.md) - quest: nothing reports whether a quest is blocked, so the start flow reconstructs it by grepping
- [PR behavioral gates](/quest/m0/pr-behavioral-gates.md) - run the applicable interop and platform gates on source PRs before merge
- [Merge evidence](/quest/m0/merge-verification-evidence.md) - bind local, CI, and device results to the current source and merge candidate
- [Verification preflight](/quest/m0/verification-preflight.md) - diagnose missing tools, denied access, and unusable runtimes before building
- [Worktree QA isolation](/quest/m0/worktree-qa-isolation.md) - give concurrent worktrees explicit bases, endpoints, and process ownership
- [Failure artifacts](/quest/m0/qa-failure-artifacts.md) - retain inspectable traces, logs, and rerun commands when QA fails
- [Browser media QA](/quest/m0/browser-media-qa.md) - measure actual playback output and lifecycle recovery beyond successful delivery
- [Transport failure drills](/quest/m0/transport-failure-drills.md) - exercise disruption, cancellation, and races with non-vacuous regressions
- [Packaged consumer QA](/quest/m0/packaged-consumer-qa.md) - install Rust and JS candidate archives outside the workspace before publication
