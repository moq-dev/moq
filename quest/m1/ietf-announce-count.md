# [M] An IETF namespace subscription says how many routes it replays

## Goal

A moq-transport session that negotiates a new extension learns the size of
the initial set a SUBSCRIBE_NAMESPACE replays, the way `AnnounceOk.active`
tells a moq-lite session, so the announce `Live` marker fires on the count
instead of the settle timer. Without the extension the timer stays.

## Plan

- Specify the extension in a `drafts/` document (a new one, or an existing
  lcurley extension draft if one fits), with its setup parameter and where
  the count rides, and validate with `just drafts check`.
- Implement it in `rs/moq-net`'s IETF publisher and subscriber; mirror in
  `js/net` if its IETF path carries announces.
- Test: a negotiated session clears on the count with N routes and with none;
  an un-negotiated one still clears on the timer.

Public API: none. Wire: a new opt-in extension.

## Required

- [Caught up](/quest/m1/cli-inspect/caught-up.md) - the per-source clear this feeds
