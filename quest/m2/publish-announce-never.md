# [XS] Drop moq-publish's announce="never"

## Goal

`@moq/publish` offers only announce modes that can be watched. A broadcast
that is never announced has no route at the relay, so only a peer that
already shares the intent to publish it could subscribe.

## Plan

Raised 2026-10-19 by the maintainer (#5703): a broadcast that never
announces can only work when client and server pre-share the intent to
publish. In `@moq/publish` it does not even do that: the catalog track is
created on the first announce (`js/publish/src/broadcast.ts`), so "never"
serves nothing.

**Open decision:** delete `announce="never"` (breaking; `MIGRATE.md` entry
in `js/publish`), keeping `source` and `always`, or keep it and document
that it is unreachable through a relay. Recommended: delete. A broadcast that
should stay unlisted uses a `.`-prefixed hidden path, which is left out of
discovery but still subscribable by name; document that on the `announce`
attribute either way.

Public API: `@moq/publish`'s `announce` attribute and property.
Also check `demo/web` for uses.

## Closes

- [#5703](https://github.com/moq-dev/moq/issues/5703) - Announce is required to be subscribed by moq-watch
