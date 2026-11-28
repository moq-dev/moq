# [XS] Drop the ignored TypeScript DOM library override

## Goal

The `js/*` packages and `.fallowrc.jsonc` stop carrying an
`@typescript/lib-dom` override that TypeScript 7 no longer reads.

## Plan

Decided 2026-11-26 (maintainer, /quest-plan of moq-dev/hang.live's
follow-ups), found by its dependency audit
([moq-dev/hang.live#56](https://github.com/moq-dev/hang.live/pull/56)):
under TypeScript 7.0.2, `tsc --listFiles` still loads the built-in DOM
library, not `@types/web`, so the `npm:@types/web` alias in the eight
`js/*/package.json` files changes nothing.

- Confirm with `tsc --listFiles` in one package before and after removal.
- Remove the dev dependency from every `js/*` package and its entry in
  `.fallowrc.jsonc`, then typecheck every package. If a newer DOM type is
  still needed, declare it where it's used instead of keeping the override.

Public API: none. Wire: none.
