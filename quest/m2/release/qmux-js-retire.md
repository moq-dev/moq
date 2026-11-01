# [XS] Retire the web-transport JS qmux

## Goal

After the first `qmux` release built from `js/qmux` is on npm,
`@moq/qmux` and `@moq/web-socket-stream` are deprecated on npm with a pointer
to `qmux`, and moq-dev/web-transport no longer publishes `@moq/qmux`, so
one repository owns the JS QMux package.

Check with `npm view qmux repository` (names moq-dev/moq),
`npm view @moq/qmux deprecated`, and `npm view @moq/web-socket-stream deprecated`.

## Plan

Decided 2026-10-20 (maintainer): moq-dev/web-transport stops publishing
`@moq/qmux` once moq's first release of the JS package is on npm. Decided
2026-11-01 (maintainer): that package is [renamed to
`qmux`](/quest/m0/qmux-npm-rename.md), so both old names are deprecated in
its favor. Waiting on that publication and a maintainer with npm rights:

- `npm deprecate @moq/qmux "Renamed to qmux; see https://github.com/moq-dev/moq/blob/main/js/qmux/MIGRATE.md"`.
- `npm deprecate @moq/web-socket-stream "Folded into qmux; see https://github.com/moq-dev/moq/blob/main/js/qmux/MIGRATE.md"`.
- Remove or freeze `js/qmux` and `js/web-socket-stream` in moq-dev/web-transport,
  whose release script would otherwise publish a competing `@moq/qmux`.
- The JSR `@moq/qmux` gets no more releases; mark it deprecated on JSR too.
