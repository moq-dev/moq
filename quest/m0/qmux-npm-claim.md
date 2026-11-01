# [XS] Claim the npm name qmux

## Goal

Condition: the unscoped npm package `qmux` exists, is owned by the moq-dev
maintainers, and has a trusted publisher for `moq-dev/moq`, workflow
`release-js.yml`, environment `npm`. Then the cut's release workflow can
publish the [renamed `js/qmux`](/quest/m0/qmux-npm-rename.md) under it. A
maintainer does this by hand. Delete this quest once these checks pass:

- `npm view qmux maintainers` lists the maintainer account.
- The package's npm settings show that trusted publisher.

## Plan

Decided 2026-11-01 (maintainer): `@moq/qmux` is renamed to `qmux` because
QMux is not MoQ-specific, and the name is claimed now, in m0, before somebody
else takes it. On 2026-11-01 `npm view qmux` returned 404 and this machine had
no npm login, so no agent can claim it.

As with [the other new names](/quest/m2/release/npm-first-publish.md),
publish a minimal `0.0.0` placeholder (README only, pointing at
`https://github.com/moq-dev/moq/tree/main/js/qmux`) to claim it, then attach
the trusted publisher. The cut publishes the real version through the
workflow with provenance. `release.ts` compares against the `latest` tag, so
the placeholder does not suppress that publish. No JSR name is needed: the
renamed package sets `"jsr": false`.

Public API: none. Wire: none.
