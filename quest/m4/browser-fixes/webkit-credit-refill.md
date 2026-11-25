# [L] A WebKit patch refills WebTransport stream and data credit

## Goal

[WebKit bug 319818](https://bugs.webkit.org/show_bug.cgi?id=319818), where
Safari never refills MAX_DATA and MAX_STREAMS credit so a long WebTransport
session stalls, is either fixed by a WebKit patch ready for the maintainer to
submit, or shown to live in Apple's closed-source Network.framework, with a
report drafted for the maintainer to send through Feedback Assistant.
`js/net` sends every WebKit browser over the WebSocket fallback until this
ships.

## Hosts

- macos - WebKit's Cocoa port and Network.framework run only there

## Plan

WebKit implements WebTransport on Network.framework
(`Source/WebKit/NetworkProcess/webtransport/cocoa/`): QUIC, and so the credit
updates, live in the closed framework, while WebKit opens one `nw_connection`
per stream and decides when to read, abort, and cancel it. So diagnose first:
recommended, check whether WebKit leaves a finished stream's `nw_connection`
uncancelled or its reads unconsumed, which would keep Network.framework from
retiring the stream and returning its credit. If WebKit releases everything and
the credit still never returns, the bug is Apple's, and this quest ends with
the drafted report instead of a patch.

Reproduce with the raw WebTransport case from
[#2388](https://github.com/moq-dev/moq/issues/2388): about 7,600 eleven-byte
unidirectional streams, and about 16 MiB of data. As of 2026-10-22 the bug was
NEW with a 2026-10-18 comment reproducing it on a newer Safari; no WebKit pull
request named it on 2026-11-25. Check both before starting, and delete this
quest if Apple already fixed it.

Decided 2026-11-25 (maintainer, /quest-plan): ✅ write the fix ourselves; the
epic's Plan says what this delivers and how it reaches upstream.

## Related

- [Safari ships the WebKit 319818 fix](/quest/m5/webkit-319818.md) - the release wait this shortens
