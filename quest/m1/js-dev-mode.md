# [S] JS packages have no dev mode, and announced requests share no subscription

## Goal

`@moq/net` and `@moq/signals` stop reading `import.meta.env` at all, so no
app environment variable can reach a bundle through them and no runtime
counts as "dev". A server that issues hundreds of
`origin.request({ announced: true })` keeps serving.

## Plan

`js/net/src/util/log.ts` reads `import.meta.env` whole, so Vite inlines every
`VITE_*` variable, and `js/signals/src/index.ts` defines dev differently. The
signals tripwire throws on the 100th subscriber in dev, and
`js/net/src/origin.ts` subscribes every announced request to one shared
discovery signal.

Decided (2026-10-04):

- Delete `dev()` from @moq/net. It gates four log lines in `connect.ts`,
  which become unconditional `console.debug`.
- Delete the signals subscriber tripwire and its dev check. Leaked effects are
  already caught by the FinalizationRegistry. Update `doc/lib/js/signals.md`,
  which still describes the tripwire.
- No discovery subscription per request: keep an `announced` count on the
  request slot, compute blindness where requests are forwarded, and add
  `sessions` to the origin's existing change race. That deletes a
  subscription, a callback, and a lifecycle.

Tests: 200 announced requests on one origin hold no subscriptions; a grep
lint fails on `import.meta.env` under `js/net` and `js/signals`.

## Closes

- [#4774](https://github.com/moq-dev/moq/issues/4774) - close this issue when the quest finishes
- [#4775](https://github.com/moq-dev/moq/issues/4775) - close this issue when the quest finishes
