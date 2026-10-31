# 2026-10-27 security audit: pre-cut findings

## Goal

Fix the 2026-10-27 external security audit's verified findings that are not
urgent for `release`, before main is promoted: relay and wire boundaries,
importer memory, moq-c handle exhaustion, browser input checks, and CI
supply-chain pins.

## Plan

An external AI audit at
[651c30fee9](https://github.com/moq-dev/moq/commit/651c30fee9) reported 8
Medium and 28 Low findings from source review (no fuzzing or live attack).
Every claim was re-checked against
[8d2fa6653a](https://github.com/moq-dev/moq/commit/8d2fa6653a) before
planning; several were wrong or proposed fixes this repository forbids.

Placed elsewhere (decided 2026-10-27, maintainer):

- Credential leaks also in `release`:
  [m0 security audit](/quest/m0/security-audit/README.md).
- lite-07 range-list scan (NET-1): owned by
  [fill-linear](/quest/m1/subscribe-ranges/fill-linear.md).
- qmux buffering a peer-declared size (NET-3): owned by
  [qmux receive lease](/quest/m2/qmux-receive-lease.md).
- Media parser fuzzing: owned by [media fuzz](/quest/m2/media-fuzz.md).
- E2EE cloned-process rule and CryptoKey length: documented in the
  E2EE draft and the `moq-e2ee` and `@moq/e2ee` docs.

Dropped, with reasons:

- SUP-3 (`||` in release gates): wrong; deliberate, and `just gh ffi
  completed` checks each language's own publisher.
- RELAY-9 (Unix socket 0666): documented, sessions still authenticate.
- RELAY-10 (pinned peers ignored by `client_ca`): unreachable from config.
- RELAY-4 (no Origin check): no ambient credentials to ride.
- RELAY-5 (connection, handshake, and auth caps): refusal constants, which
  AGENTS.md forbids; handshakes already time out.
- MUX-1 (HLS whole-resource fetch): the origin is operator-chosen, a byte cap
  is a refusal constant, and whole segments are buffered on purpose.
- MUX-4 (`file:` playlists read any local file): intended; confining to the
  playlist directory breaks ffmpeg layouts.
- JS-1 (demo player per announcement): the demo's purpose.
- AUTH-2 (no required expiry): an opt-in policy by design.
- AUTH-5 (tokens in URL before lite-07): inherent until lite-07 publishes;
  its log exposure is fixed by [log secrets](/quest/m0/security-audit/log-secrets.md).
- SUP-5 (libav integrity), SUP-10 (CODEOWNERS, pin lint): no real gain.
- SUP-9 (one GPG key): moving `SIGNING_KEY` into a protected Environment is
  a maintainer settings change, not a quest.
- The audit's own fixes for MUX-3 (would break large MKV imports) and E2EE-A
  (a random epoch is copied by a fork too) were wrong; the children use
  better ones.

## Required

- [fMP4 import streams mdat](/quest/m1/security-audit/fmp4-stream-mdat.md) - a box header declaring a huge size cannot pin importer memory across segments
- [MKV import streams blocks](/quest/m1/security-audit/mkv-stream-blocks.md) - a declared block, metadata master, or ignored element cannot pin importer memory
- [Relay HTTP surface](/quest/m1/security-audit/relay-http.md) - `/fetch` cannot be sniffed as active content, and no web page can reach the ops listener
- [Browser input checks](/quest/m1/security-audit/js-input.md) - room metadata is schema-validated and the http:// fingerprint fetch is aborted and length-checked
- [CI tool pins](/quest/m1/security-audit/ci-pins.md) - binding generators are pinned by commit in one place, and the remaining unpinned installs and broad token grants are fixed
