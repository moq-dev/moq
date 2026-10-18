# [XS] Backport the qmux credit and close pin

## Goal

The current release uses a patched qmux 0.5.x that returns unread stream
credit and delivers its close frame. It stays on web-transport-trait 0.4.

## Plan

Once the upstream 0.5 fix ships, update the release-line pin and run the
WebSocket and TLS transport checks. Follow the backport workflow. This is
independent of the 0.6 main pin and does not gate the next breaking cut.

Public API: none. Wire: none.

## Required

- [qmux 0.5 publication](/quest/m0/qmux-credit-backport-upstream.md) - the release-line patch is available

## Related

- [qmux credit](/quest/m0/qmux-credit.md) - shared diagnosis and the main pin
