# [S] Decide whether JS sends IETF FETCH

## Goal

A recorded decision on whether `@moq/net` gets an IETF FETCH requester, with
the use cases that would need one. If yes, this quest is rewritten into the
implementation or replaced by its quest; if no, the reason is recorded where
the JS IETF code says it rejects fetch streams.

## Plan

Planned 2026-10-28 from [moq-dev/moq#7051](https://github.com/moq-dev/moq/pull/7051):
JS answers IETF FETCH but never sends one or requests a fill, and
`js/net/src/ietf/connection.ts` rejects incoming fetch streams, so JS can
fetch only over moq-lite. A requester would map `FetchGroupOptions.maxDelay`
to FILL_TIMEOUT, decode End of Range markers (`0x8C`, `0x10C`, `0x20C`), and
accept fetch streams. It would also allow JS-to-Rust FETCH interop in that
direction.

Survey the cases first: a browser viewer against a moq-transport-only relay,
the interop runner's moq-dev-js client, and the JS ranges work. Present the
decision with a recommendation; implementation is a separate quest.

Public API: none until decided. Wire: none until decided.
