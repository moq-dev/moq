# [S] Browser publishers adapt to their send estimate

## Goal

A browser publisher on shipping Chrome and Firefox gets a send-rate estimate, so its bandwidth reservations (`Reservation.rate`) follow the path and `@moq/publish` encoders adapt, instead of holding their ceiling because the estimate is always `undefined`.

The WebSocket fallback is out of scope: it has no browser estimate to read, so its publishers keep holding their rate (decided 2026-11-07, below). That includes every WebKit browser, which `js/net` routes to the fallback (`isWebTransportUserAgentSupported` in `js/net/src/connection/browser.ts`) until [Safari WebTransport](/quest/m5/safari-webtransport.md) lifts the gate; that quest owns Safari's estimate.

## Plan

Moved to m3 2026-11-12 (maintainer, /quest-audit), with the probe epic: no API or wire change, and it waits on Chrome, so the cut does not wait on it.

Found by [moq-dev/moq#7524](https://github.com/moq-dev/moq/pull/7524): `js/net`'s `Allocator` only reads `WebTransport.getStats()`'s `estimatedSendRate`, which shipping Chromium lacks (it is behind `--enable-blink-features=WebTransportStats`). qmux reports no counters, and WebRTC's `getStats` throws.

Found 2026-11-05 ([moq-dev/moq#8016](https://github.com/moq-dev/moq/pull/8016)), replacing the 2026-11-01 plan to feed the peer's PROBE estimate to the send allocator:

- **PROBE can't serve the uplink.** The PROBE opener reads the peer's *send* estimate toward it, so `Probe.estimatedRecvRate` is our downlink, and Rust names it `recv_bandwidth`. No wire message carries what the relay receives from us. The old doc comment said the reverse, which is where the plan came from; [moq-dev/moq#8016](https://github.com/moq-dev/moq/pull/8016) fixed it. Feeding the downlink to the send allocator would pace an uplink to the downlink's capacity.
- **The browsers are filling the gap.** Firefox 158 ships `getStats()` with `estimatedSendRate` ([mdn/browser-compat-data#30695](https://github.com/mdn/browser-compat-data/pull/30695)). Chrome 159 intends to ship it ([Intent to Ship](http://www.mail-archive.com/blink-dev@chromium.org/msg17946.html), sent 2026-11-05), with a null estimate until one exists. Safari 26.4 has `getStats()`, but it is a stub: `estimatedSendRate` is always null, and `smoothedRtt` and every counter are always 0. The send allocator handles a null estimate with no code change: it reads `undefined` and holds the rate.

Decided 2026-11-06 (maintainer, /quest-unblock): how `js/net` treats Safari's stub `getStats()`.

- (a) Read a `smoothedRtt` of 0 as absent wherever it is read; Safari is still sampled and its 0 counters still reach `Stats`.
- (b) ✅ At setup, treat a `getStats()` whose `smoothedRtt` is 0 as no `getStats()` at all, so Safari advertises PROBE `None`, is never sampled, and reports no counters. The rule lifts once WebKit reports a real RTT.
- (c) Leave it until WebKit fills the fields.

Reason: one rule decided at setup fixes the RTT, the polling, and the counters. [moq-dev/moq#8016](https://github.com/moq-dev/moq/pull/8016) implements it (`inspect` and `statsOf` in `js/net/src/connection/stats.ts`).

What remains once the waits clear:

- In stable Chrome (no flag) and Firefox, verify through a shaped link that a publisher's bitrate follows a capacity drop. Reuse the [probe epic](/quest/m3/probe/README.md)'s shaped-uplink harness if it has landed.
- The WebSocket fallback has no estimate whatever the browsers ship, so its publishers still hold their rate, as decided below.

Decided 2026-11-07 (maintainer, /quest-unblock): Safari publishers and the WebSocket fallback get no estimate before WebKit reports one.

- ✅ (a) Wait for WebKit, as today.
- (b) A lite-07 receiver report, where the relay sends back the rate it receives from us, which can only lower the estimate while app-limited.
- (c) Estimate locally from how fast the transport drains our writes, which depends on each browser's buffering.

Reason: Chrome and Firefox need nothing. For Safari, (b) and (c) each build an app-layer controller above the browser's own that a later WebKit release makes redundant. For the WebSocket fallback, (b) or (c) would be the only source, since it never reads WebTransport stats; building one waits until fallback adaptation is worth it on its own.

Public API: none expected. Wire: none.

## Waiting

- Chrome: ships `WebTransport.getStats()` with `estimatedSendRate` enabled for all users in 159, per the 2026-11-05 Intent to Ship. Check that stable Chrome's `getStats()` reports a non-null `estimatedSendRate` on a connection that has been sending media for a few seconds.

## Related

- [Padding probes](/quest/m5/probe-padding.md) - raises the estimate once one exists
- [Media stats](/quest/m3/stats/README.md) - its JS stats read PROBE for the downlink
