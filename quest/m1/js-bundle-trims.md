# [M] JS bundle trims

## Goal

The watch and publish elements stop shipping bytes a consumer's bundler
cannot remove. Measured in bundled, minified form (2026-09-26):

- The inlined worklets are built unminified, and code inside a string can't
  be minified by the consumer. The watch render worklet is 25.9 KB against
  10.1 KB minified, and the publish capture worklet is 4.7 KB against 2.3 KB.
- `bowser` costs 37 KB for three checks in
  `js/net/src/connection/browser.ts`: the WebKit engine, iOS, and Firefox 153
  or later.
- `@moq/flate` imports all of pako (42 KB) where the deflate and inflate
  halves have their own entry points.
- `@moq/qmux` (39 KB, the WebSocket fallback) and `media-captions` (15 KB,
  watch only) load eagerly even when unused.

## Plan

Decided in planning: all four trims are in scope. Mediabunny is its own
quest.

Guidance:

- Worklets: minify them in `js/common/vite-plugin-worklet.ts`.
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

## Related

- [Publish lazy file source](/quest/m1/publish-lazy-file.md) - the largest JS saving, landed separately
- [Size report](/quest/m1/size-report.md) - tracks these entries nightly
