# [S] One JS dev mode, a warning tripwire, and one discovery subscription

## Goal

`@moq/net` and `@moq/signals` agree on what dev mode is and read it without
pulling the whole `import.meta.env` object into a bundle. The signals
subscriber tripwire warns, as `doc/lib/js/signals.md` says, instead of
throwing. A server that issues hundreds of `origin.request({ announced: true })`
keeps serving, in dev or not.

## Plan

Decided (2026-10-04), one quest because #4774 and #4775 share the dev
definition:

- One shared helper reads `import.meta.env?.DEV`, `import.meta.env?.MODE`, and
  `globalThis.process?.env?.NODE_ENV` property by property, so Vite inlines
  only those keys. `js/net/src/util/log.ts` and `js/signals/src/index.ts` both
  use it. The signals `typeof import.meta.env` check is a bare reference too.
- The tripwire in `Signal.subscribe` logs a warning instead of throwing.
- `js/net/src/origin.ts` subscribes each request to the shared discovery
  signal. Use one discovery subscription per origin that recomputes across
  its requests, so the count stops growing with requests.

Tests: 200 announced requests on one origin add one subscriber; a bundle built
with an extra `VITE_*` variable does not contain it.

## Closes

- [#4774](https://github.com/moq-dev/moq/issues/4774) - close this issue when the quest finishes
- [#4775](https://github.com/moq-dev/moq/issues/4775) - close this issue when the quest finishes
