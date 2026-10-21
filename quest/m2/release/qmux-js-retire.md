# [XS] Retire the web-transport JS qmux

## Goal

After the first `@moq/qmux` release built from `js/qmux` is on npm,
`@moq/web-socket-stream` is deprecated on npm with a pointer to
`@moq/qmux`, and moq-dev/web-transport no longer publishes `@moq/qmux`, so
one repository owns the npm name.

Check with `npm view @moq/qmux repository` (names moq-dev/moq) and
`npm view @moq/web-socket-stream deprecated`.

## Plan

Decided 2026-10-20 (maintainer): moq-dev/web-transport stops publishing
`@moq/qmux` once moq's first release of it is on npm. Waiting on that
publication and a maintainer with npm rights:

- `npm deprecate @moq/web-socket-stream "Folded into @moq/qmux; see https://github.com/moq-dev/moq/blob/main/js/qmux/MIGRATE.md"`.
- Remove or freeze `js/qmux` and `js/web-socket-stream` in moq-dev/web-transport,
  whose release script would otherwise publish a competing `@moq/qmux`.
