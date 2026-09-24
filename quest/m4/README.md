# m4: upstream

## Goal

Work waiting on an upstream release or external dependency to ship, kept here
so it is not forgotten.

## Plan

Each quest states its gate as a plain-text `Required` bullet. Re-check the
gates periodically; when one clears, remove the bullet and promote the quest to
the milestone its priority belongs in.

## Quests

- [VAAPI encode and decode](/quest/m4/video-vaapi.md) - DMA-BUF encode, H.265 decode, and pre-generated bindings that remove the libclang build dependency, all gated on a moq-dev/vaapi release
- [#2907](/quest/m4/2907-bind-the-browser-through-moq-ffi-uniffi-instead-of-a.md) - the browser reaches moq-ffi through a generated TypeScript binding once a JS generator is stable
- [Safari WebTransport](/quest/m4/safari-webtransport.md) - WebKit browsers return to WebTransport once WebKit 319818 ships fixed
- [MSFTS convergence](/quest/m4/msfts-convergence.md) - the demultiplexed TS lane maps onto MSFTS ES-level carriage once msfts#33 settles the payload unit
