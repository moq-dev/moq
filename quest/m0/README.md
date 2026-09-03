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
- [Rust log redaction](/quest/m0/rust-log-redaction.md) - moq-native logs relay URLs with their query and moq-rtmp logs stream keys
- [#2849](/quest/m0/2849-moq-import-ts-a-truncated-or-spliced-opus-pes-ends-the.md) - moq import ts: a truncated or spliced Opus PES ends the session
- [#3139](/quest/m0/3139-moqsrc-a-rendition-nobody-answers-keeps-the-session-alive.md) - moqsrc: a rendition nobody answers keeps the session alive after the catalog closes
- [#2981](/quest/m0/2981-moq-audio-nothing-in-the-decode-or-playback-path-models-a.md) - moq-audio: a media gap is a hole in the output, not a splice
- [#3080](/quest/m0/3080-fix-watch-audio-ring-truncate-can-race-the-worklet-reader.md) - watch: an audio ring truncate can race the worklet reader for one quantum
- [#2833](/quest/m0/2833-moq-export-ts-a-rewound-timeline-stalls-the-si-table.md) - moq export ts: a rewound timeline stalls SI tables, PCR, and pacing until the media clock catches up
- [#2806](/quest/m0/2806-js-net-the-draft-14-15-adapter-keeps-one-request-per.md) - js/net: a duplicate PUBLISH_NAMESPACE on draft-14/15 strands the first request
- [Windows window capture](/quest/m0/windows-window-capture-blank.md) - Windows window capture returns black pixels for GPU-composited windows
- [Capture device loss](/quest/m0/capture-device-loss.md) - an AVFoundation camera that disappears parks the reader forever
- [X11 window identity](/quest/m0/x11-window-identity.md) - a reused X11 window id can publish an unrelated window
- [Window capture lifecycle](/quest/m0/capture-window-lifecycle.md) - a minimized or resizing window ends capture instead of riding it out
- [#2799](/quest/m0/2799-moq-video-capture-negotiates-twice-so-a-window-resize.md) - moq-transcode: the ladder follows a source resolution change instead of keeping the one it started with
- [Group charge](/quest/m0/group-charge.md) - charge real per-group cost so MOQ_CACHE_CAPACITY bounds real memory
- [H.265 suffix SEI ownership](/quest/m0/h265-suffix.md) - suffix SEI stays with the access unit it follows, including at EOF
- [#2676](/quest/m0/2676-libmoq-process-exit-can-abort-in-glibcs-pthread-tpp.md) - libmoq: process exit can abort in glibc's __pthread_tpp_change_priority
- [#3123](/quest/m0/3123-moq-bench-a-lagged-group-permanently-ends-the.md) - moq-bench: a lagged group ends the subscription, so offered load silently decays mid-run
- [#2838](/quest/m0/2838-js-flate-codec-rejects-frames-that-inflate-past-the.md) - js/flate: the inflate-cap test times out under parallel load
- [#2798](/quest/m0/2798-moq-import-ts-an-audio-resync-is-silent-no-log-no-counter.md) - moq import ts: an audio resync leaves no trace, no log, no counter
- [#2860](/quest/m0/2860-cpp-obs-moq-source-cpp-has-no-test-coverage.md) - cpp/obs: moq-source.cpp has no test coverage
- [#2868](/quest/m0/2868-obs-the-plugin-targets-obs-31-1-1-while-linux-ci-links.md) - obs: the plugin targets OBS 31.1.1 while Linux CI links against 32
