# m0: bug fixes

## Goal

Defects in what main ships today: crashes, races, protocol violations,
wrong output, security gaps, and the missing tests that let them through.

## Plan

Fix on main. Security and credential exposure lead; user-visible breakage
next; hardening, tooling, and test debt close the list. Each fix lands with a
regression test per Root Cause First.

## Quests

- [#3087](/quest/m0/3087-relay-mtls-peers-bypass-auth-api-mode-so-proxy-grants.md) - relay: mTLS peers bypass --auth-api-mode, so proxy grants can't refuse or scope them
- [#2405](/quest/m0/2405-js-net-connect-logs-on-every-connection-at-the-wrong.md) - js/net: connect() logs on every connection at the wrong level and prints the JWT in the URL
- [Dart leaks](/quest/m0/dart-leak.md) - the generated Dart bindings leak native memory on every call
- [#3207](/quest/m0/3207-send-valid-publish-done-statuses-for-every-supported-ietf.md) - Send valid PUBLISH_DONE statuses for every supported IETF draft
- [#3076](/quest/m0/3076-moq-relay-publish-is-unimplemented-by-design-make-the.md) - moq-relay: PUBLISH is unimplemented by design - make the rejection fail fast for clients that wait
- [#2388](/quest/m0/2388-safari-stops-delivering-new-incoming-unidirectional.md) - Safari stops delivering new incoming unidirectional streams after roughly 7000 on a session; one stream per group exhausts that in about two minutes of playback
- [#2527](/quest/m0/2527-publish-video-breaks-when-the-publishers-window-is.md) - Publish video breaks when the publisher's window is minimized, on every browser using the MediaStreamTrackProcessor polyfill
- [#2849](/quest/m0/2849-moq-import-ts-a-truncated-or-spliced-opus-pes-ends-the.md) - moq import ts: a truncated or spliced Opus PES ends the session
- [#2788](/quest/m0/2788-moq-transcode-run-cant-bootstrap-from-a-demand-gated.md) - moq-transcode: run() can't bootstrap from a demand-gated source that doesn't advertise its geometry
- [#3139](/quest/m0/3139-moqsrc-a-rendition-nobody-answers-keeps-the-session-alive.md) - moqsrc: a rendition nobody answers keeps the session alive after the catalog closes
- [#2812](/quest/m0/2812-watch-has-audio-stutter-on-ios-on-https-moq-dev-watch.md) - Watch has Audio stutter on iOS on https://moq.dev/watch/
- [#2981](/quest/m0/2981-moq-audio-nothing-in-the-decode-or-playback-path-models-a.md) - moq-audio: nothing in the decode or playback path models a media gap
- [#3080](/quest/m0/3080-fix-watch-audio-ring-truncate-can-race-the-worklet-reader.md) - fix(watch): audio ring truncate can race the worklet reader for one quantum
- [#2833](/quest/m0/2833-moq-export-ts-a-rewound-timeline-stalls-the-si-table.md) - moq export ts: a rewound timeline stalls the SI table cadence until the media clock catches up
- [#2806](/quest/m0/2806-js-net-the-draft-14-15-adapter-keeps-one-request-per.md) - js/net: the draft-14/15 adapter keeps one request per namespace, so a duplicate strands the first
- [#2799](/quest/m0/2799-moq-video-capture-negotiates-twice-so-a-window-resize.md) - moq-video: capture negotiates twice, so a window resize between the probe and the first subscriber strands consumers that fixed on the first snapshot
- [#2813](/quest/m0/2813-capture-on-ios-is-software-only-not-hardware.md) - Capture on iOS is software only , not hardware
- [#2847](/quest/m0/2847-the-quinn-backends-send-bandwidth-estimate-is-cwnd-rtt.md) - The quinn backend's send-bandwidth estimate is cwnd/rtt, not a rate
- [Group charge](/quest/m0/group-charge.md) - charge real per-group cost so MOQ_CACHE_CAPACITY bounds real memory
- [H.265 suffix SEI ownership](/quest/m0/h265-suffix.md) - suffix SEI stays with the access unit it follows, including at EOF
- [#2676](/quest/m0/2676-libmoq-process-exit-can-abort-in-glibcs-pthread-tpp.md) - libmoq: process exit can abort in glibc's __pthread_tpp_change_priority
- [#2850](/quest/m0/2850-js-net-give-reader-a-synchronous-decode-so-the-publisher.md) - js/net: give Reader a synchronous decode so the publisher need not read controls ahead
- [#3123](/quest/m0/3123-moq-bench-a-lagged-group-permanently-ends-the.md) - moq-bench: a lagged group permanently ends the subscription, so offered load silently decays mid-run
- [#2838](/quest/m0/2838-js-flate-codec-rejects-frames-that-inflate-past-the.md) - js/flate: "codec rejects frames that inflate past the default cap" times out under parallel test load
- [#3115](/quest/m0/3115-moqsink-the-publication-has-no-generation-so-a-flush.md) - moqsink: the publication has no generation, so a flush after EOS cannot restart it
- [#2798](/quest/m0/2798-moq-import-ts-an-audio-resync-is-silent-no-log-no-counter.md) - moq import ts: an audio resync is silent - no log, no counter, no downstream signal
- [#2860](/quest/m0/2860-cpp-obs-moq-source-cpp-has-no-test-coverage.md) - cpp/obs: moq-source.cpp has no test coverage
- [#2868](/quest/m0/2868-obs-the-plugin-targets-obs-31-1-1-while-linux-ci-links.md) - obs: the plugin targets OBS 31.1.1 while Linux CI links against nixpkgs' 32.1.2
- [#2067](/quest/m0/2067-test-open-gop-h-264-tune-in-end-to-end-leading-picture.md) - Test open-GOP H.264 tune-in end to end (leading-picture handling)
- [#1095](/quest/m0/1095-avoid-allocation-in-axum-tungstenite-websocket-message.md) - Avoid allocation in axum/tungstenite WebSocket message conversion
