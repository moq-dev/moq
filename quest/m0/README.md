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
- [Audio identity](/quest/m0/3479-watch-audio-identity.md) - js/watch: a catalog republish rebuilds the audio graph only when the decoder identity changes
- [Encoder lag](/quest/m0/publish-audio-lag-measure.md) - js/publish: measure the audio encoder's input-to-output lag on a high-RTT path
- [IETF error codes](/quest/m0/ietf-error-codes.md) - every code on a moq-transport wire is a registered value for the negotiated draft, requests and stream resets alike
- [Resume info](/quest/m0/resume-info-newest.md) - moq-net: resume reports segment zero's track info, so a replaced broadcast rescales timestamps on the predecessor's timescale
- [#3361](/quest/m0/3361-js-every-moq-package-a-package-imports-is-declared.md) - js: every @moq package a package imports is a declared dependency
- [TS timebase discontinuity](/quest/m0/ts-forward-discontinuity.md) - preserve source-signalled clock changes through import and export
- [Group charge](/quest/m0/group-charge.md) - charge real per-group cost so MOQ_CACHE_CAPACITY bounds real memory
- [uring all-features](/quest/m0/uring-all-features-build.md) - moq-uring does not compile with `--all-features`, so the nightly features gate fails on it
- [Retirement race](/quest/m0/transcode-retirement-race.md) - moq-transcode: retirement has no coverage for a fetch that is still opening its decoder
- [PR behavioral gates](/quest/m0/pr-behavioral-gates.md) - run the applicable interop and platform gates on source PRs before merge
- [Verification preflight](/quest/m0/verification-preflight.md) - diagnose missing tools, denied access, and unusable runtimes before building
- [Worktree QA isolation](/quest/m0/worktree-qa-isolation.md) - give concurrent worktrees explicit bases, endpoints, and process ownership
- [Failure artifacts](/quest/m0/qa-failure-artifacts.md) - retain inspectable traces, logs, and rerun commands when QA fails
- [Browser media QA](/quest/m0/browser-media-qa.md) - measure actual playback output and lifecycle recovery beyond successful delivery
- [Transport failure drills](/quest/m0/transport-failure-drills.md) - exercise disruption, cancellation, and races with non-vacuous regressions
- [Packaged consumer QA](/quest/m0/packaged-consumer-qa.md) - install Rust and JS candidate archives outside the workspace before publication
