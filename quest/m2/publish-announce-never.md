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

Decided 2026-10-19 by the maintainer: delete `announce="never"`, keeping
`source` and `always`. It is a breaking change in
`js/publish`, and the Impact bullet says how to migrate. A broadcast that should stay unlisted uses a `.`-prefixed
hidden path, which is left out of discovery but still subscribable by name;
document that on the `announce` attribute.

Public API: `@moq/publish`'s `announce` attribute and property.
Also check `demo/web` for uses.
