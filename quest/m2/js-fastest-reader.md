# [M] JS fastest-reader pacing

## Goal

`@moq/net` paces a group's upstream stream by its fastest reader like the
native path: it stops reading once a window past the fastest reader is
buffered, so stream flow control pushes back to the publisher, on WebTransport
and on the JS qmux receiver.

## Plan

Planned 2026-11-02 (maintainer, follow-up planning after the 2026-11-02 /quest-complete run): JS parity moves out of the m1
[fastest-reader quest](/quest/m1/unbounded-groups/fastest-reader.md), so the m1
cut does not wait on browsers. The JS qmux receiver could pace today but
follows the browser, so one design covers both.

## Required

- [Browsers expose WebTransport receive credit](/quest/m2/browser-receive-credit.md) - the page can tell how much credit an unread stream holds
