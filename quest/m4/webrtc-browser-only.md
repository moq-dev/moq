# [S] The vendored stack keeps only what browsers negotiate

## Goal

The vendored str0m, dimpl, `is`, and sctp-proto drop every feature no
browser's WebRTC negotiates today, starting with dimpl's PSK cipher suites
and the `ccm` dependency they pull in, so nothing MoQ can't reach from a
browser peer stays compiled.

## Plan

Planned 2026-11-07 (maintainer, /quest-complete), a follow-up of
[moq-dev/moq#8163](https://github.com/moq-dev/moq/pull/8163). The maintainer
widened the suggestion from PSK alone: delete anything browsers don't use
now. Browser DTLS authenticates with certificates and fingerprints, never
PSK. Inventory what Chrome, Firefox, and Safari offer and accept (cipher
suites, extensions, SDP attributes, codecs' packetizers) against what the
vendored code still carries, cite the evidence for each removal in the PR,
and keep what a browser negotiates even if MoQ doesn't call it yet. The
[WebRTC epic](/quest/m3/webrtc/README.md)'s settled removals (its "What goes" list, such as G.7xx, DTMF, and
comfort noise) and non-goals (TURN, DTLS 1.3) stay as decided: this trim
only removes more, never restores. API
that moq-webrtc never calls but a browser could need stays for
[the dead-API trim](/quest/m4/webrtc-dead-api.md).

Public API: none. Wire: none toward browsers.
