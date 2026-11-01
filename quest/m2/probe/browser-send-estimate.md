# [M] Browser publishers adapt to the relay's PROBE estimate

## Goal

A browser publisher on shipping Chrome or Safari gets a send-rate estimate, so its bandwidth reservations (`Reservation.rate`) follow the path and `@moq/publish` encoders adapt, instead of holding their ceiling because the estimate is always `undefined`. Where `WebTransport.getStats()` reports an estimate, the connection keeps using it.

## Plan

Found by [moq-dev/moq#7524](https://github.com/moq-dev/moq/pull/7524), which gated `getStats()` sampling on a reservation: shipping Chromium has no `WebTransport.getStats()` (only behind `--enable-blink-features=WebTransportStats`), and neither does Safari. So `js/net`'s `Allocator` never receives an estimate there, and publishers never follow one. qmux reports no counters, and WebRTC's `getStats` throws.

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): m2, in the probe epic, feeding the peer's PROBE estimate to the send allocator. The relay already reports what it receives from us: `Probe.estimatedRecvRate` in `js/net/src/connection/stats.ts`, exposed as `Reload.probe` (`js/net/src/connection/reload.ts`).

- Source order: a transport `getStats()` estimate where the browser has one, else the peer's PROBE `estimatedRecvRate`, else `undefined` (hold the rate, as today). Settle in the PR whether a present-but-empty `getStats` counts as absent.
- A receive-rate report lags the send side by about an RTT plus the peer's report cadence, and reads what arrived rather than what the path could carry. Check how the allocator should treat it while app-limited (it can only fall or hold, not rise past what was sent), and leave raising it to [padding](/quest/m2/probe/padding.md).
- Observing `Reload.probe` requests reports from the peer; request them only while a reservation reads the estimate, matching #7524's gate.
- Test on `@moq/clock`'s `Manual` with a mock session whose transport has no `getStats`: a PROBE report reaches `Reservation.rate`, a falling report lowers it, and a reconnect clears it. Verify in shipping Chromium (no flag) through a shaped link that a publisher's bitrate follows a capacity drop.

Public API: none expected (the estimate's source is internal). Wire: none; PROBE `Report` is already sent.

## Related

- [Media stats](/quest/m2/stats/README.md) - its JS stats also read PROBE because `getStats()` is missing
