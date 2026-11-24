# [S] A browser reports a revocation's reason, not a broken Announce stream

## Goal

When a server closes a live lite-07 session after refusing its token, such as
a revoked certificate, `@moq/net` in Chrome ends the session with the
refusal (`refused: certificate revoked`) or the server's close reason, not
`ProtocolViolation("Announce Stream broke")`.

## Plan

Since [moq-dev/moq#9518](https://github.com/moq-dev/moq/pull/9518), a
WebTransport close resets every stream the server sends on with
`WT_SESSION_GONE` before its `CLOSE_WEBTRANSPORT_SESSION` capsule, to keep
Chrome's renderer alive (quest/m5/chromium-close-quiesce.md). Chrome surfaces
that reset without a code the page can tell from an application's, so
`#runAnnounce` (`js/net/src/lite/connection.ts`) sees its stream break while
the session still looks live, and closes it with code 3 before the capsule's
reason arrives. Kixel's browser E2E shows it: after a revocation, the AUTH
stream's refusal reaches the page (`auth token refused ... certificate
revoked`), but the page's session error reads `remote error: 3 (Announce
Stream broke)`. A refusal at connect is unaffected: it reads `remote error: 2
(unauthorized)`.

Chrome cannot tell that reset from an application's: Chrome 157 reports a
`WT_SESSION_GONE` reset as `WebTransportError` with `source: "stream"` and
`streamErrorCode: 0`, "Received RESET_STREAM.", the same as an application
code 0 (measured in Kixel's E2E), so `@moq/net` cannot key on the code.
Recommended: a session-fatal stream breaking after the session's token got a
final refusal ends the session with that refusal, in `#runAnnounce`, the
Announce Stream's `broken` handler, and any other session-fatal reader. The
refusal arrives first, since the server writes it before closing and the
close's resets still deliver what was written.
