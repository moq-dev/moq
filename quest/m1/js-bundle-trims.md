# [M] JS bundle trims

## Goal

The watch and publish elements stop shipping bytes a consumer's bundler
cannot remove. Measured in bundled, minified form (2026-09-26):

- `bowser` costs 37 KB for three checks in
  `js/net/src/connection/browser.ts`: the WebKit engine, iOS, and Firefox 153
  or later.
- `@moq/flate` imports all of pako (42 KB) where the deflate and inflate
  halves have their own entry points.
- `@moq/qmux` (39 KB, the WebSocket fallback) and `media-captions` (15 KB,
  watch only) load eagerly even when unused.

## Plan

Decided in planning: all three trims are in scope (worklet minification
landed with the strict-CSP worklet change). Mediabunny is its own
quest.

Guidance:

- bowser: write a small user-agent check with unit tests over real UA
  strings. Chrome's UA contains `AppleWebKit` too, and iPadOS Safari reports
  macOS, so test Chrome and Edge on macOS, iPadOS Safari, iOS Chrome, and
  Firefox 152 and 153.
- pako: measure before keeping the change. If a caller needs both halves,
  the split buys nothing.
- Lazy qmux: if connect races WebSocket against WebTransport, a lazy import
  puts module loading on the fallback path. Measure the connect time it adds,
  and drop this trim if the fallback gets noticeably slower.
- Report the before and after first-load sizes in the PR.
- Add a CI check that imports the built `@moq/watch` dist outside a browser
  (the `bun -e 'await import("./dist/index.js")'` that verified
  [#4217](https://github.com/moq-dev/moq/pull/4217)). Unit tests run on
  `src`, so a bundled browser-only dependency that breaks Node, Bun, or SSR
  imports only shows up in the dist, and these trims move exactly those
  imports around. Cover `@moq/publish` the same way if it is cheap.

## Related

- [Size report](/quest/m1/size-report.md) - tracks these entries nightly
