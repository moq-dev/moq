# [M] A Chromium patch stops a WebTransport close from crashing the tab

## Goal

A Chromium patch for [issue 589817008](https://issues.chromium.org/issues/589817008),
ready for the maintainer to submit: a `CLOSE_WEBTRANSPORT_SESSION` capsule
that arrives while the page has a bidirectional stream with a read pending
and a unidirectional stream with a write in flight closes the session instead
of killing the renderer with a null-pointer read.

## Plan

The reproduction, a 100-line aioquic server and a 30-line page with no MoQ,
is attached to the issue; run it on the unpatched and patched builds.
moq-quic works around the crash by quiescing every stream before it sends the
capsule, which costs a round trip (or a grace period) on every close;
[Send a WebTransport close's capsule without waiting](/quest/m5/chromium-close-quiesce.md)
deletes that machinery once a fix reaches Chrome stable. Check the issue's
status before starting: if Chromium already landed a fix, delete this quest
instead.

Decided 2026-11-25 (maintainer, /quest-plan): ✅ write the fix ourselves; the
epic's Plan says what this delivers and how it reaches upstream.
