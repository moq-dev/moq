# [XS] Every WebKit browser takes the WebSocket path

## Goal

js/net refuses WebTransport on every WebKit engine, not only the Safari brand.
Chrome, Firefox and Edge on iOS and iPadOS are WKWebView and hit the same WebKit
bug (319818: flow-control credit never refills, so a session dies after roughly
7,600 streams or 16 MiB), yet `isWebTransportUserAgentSupported` admits them
today and their sessions freeze after about two minutes of playback.

## Plan

`js/net/src/connection/browser.ts` gates with Bowser `satisfies({ safari: "<0" })`.
Add the engine check: a browser whose OS is iOS or iPadOS, or whose engine is
WebKit, returns false, and Firefox keeps its version gate. Tests in
`browser.test.ts` with user agents for Safari on macOS, Chrome on iOS, Firefox on
iOS, Edge on iOS, and desktop Chrome and Firefox. Extend the Safari note in
`doc/lib/js/index.md` to WebKit.

## Related

- [Safari WebTransport](/quest/m2/safari-webtransport.md) - relaxes this gate once WebKit ships the fix
- [#2388](https://github.com/moq-dev/moq/issues/2388) - the WebKit tracking issue that gate quest closes
