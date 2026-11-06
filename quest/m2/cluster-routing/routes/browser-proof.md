# [M] Browser tabs prove their route key to the relay

## Goal

Before the lite-07 cut, a lite-07 session from a current Firefox tab and a
current Chromium tab connects to `moq-relay` with its route-key proof
verified, and a nightly browser test keeps it that way.

A tab proves a key whenever its WebTransport exposes `exportKeyingMaterial`
(`js/net/src/lite/route_key.ts`). The relay derives each WebTransport
session's secret per draft-ietf-webtrans-http3 section 4.8
(`rs/moq-tokio/src/quic/web/session.rs`, `rs/moq-uring/src/quic/web.rs`), and
ends the session with a protocol violation when a proof does not verify
(`rs/moq-net/src/lite/setup.rs`). So a browser whose exporter disagrees with
the relay cannot open a lite-07 session at all. Tests compare the Rust and JS
transcript vectors and both Rust exporter ends, but no real browser exporter
has ever run against the relay
([moq-dev/moq#7937](https://github.com/moq-dev/moq/pull/7937) left this gap).

Browser support as of 2026-11-06: Firefox 155 shipped `exportKeyingMaterial`
on an older draft (context optional, output fixed at 32 bytes), and Firefox 160
(stable 2026-11-10) follows the spec's three arguments
([MDN BCD #30999](https://github.com/mdn/browser-compat-data/pull/30999)).
Chromium enabled it by default in Canary, targeting Chrome 158
([crbug 556304550](https://issues.chromium.org/issues/556304550)), and mixes
the session's CONNECT stream into the derivation.

## Plan

Decided 2026-11-06 (maintainer, /quest-complete while queueing
[moq-dev/moq#8022](https://github.com/moq-dev/moq/pull/8022)):

- ✅ **Cover Firefox and Chromium**, and treat a mismatch as a bug that
  blocks the cut. It joins the Routes epic, which gates the cut. Not chosen:
  Firefox only until Chrome 158 ships; not planning it.
- ✅ **A nightly Playwright lane.** Firefox and Chromium tabs in the interop
  suite (`test/interop`, nightly and the release gate) dial a relay on
  lite-07 and assert the relay accepted the tab's proven key. A browser
  build without the exporter fails the lane loud rather than passing
  unproven. Not chosen: a one-time manual check before the cut.
- ✅ **A browser whose exporter cannot match sends no proof.** If Firefox
  155-159's older-draft export differs from the relay's derivation, the JS
  client tells it apart at setup from the exporter's shape (the old draft
  takes two arguments, the spec three) and sends no proof there, so the tab
  connects unproven. Never send a proof the relay will reject. Not chosen:
  requiring Firefox 160+; ignoring 155-159.

Start by running both browsers against a local relay and comparing the
exported bytes with the relay's for the same session. Fix any mismatch at the
root cause, on whichever side departs from the draft. Pin the Playwright
browser builds that carry the exporter, and record their versions here.
