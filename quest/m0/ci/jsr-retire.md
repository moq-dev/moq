# [XS] Archive the @moq packages on JSR

## Goal

The `@moq` packages on jsr.io are archived with a pointer to npm, so nothing
can publish to them again. Deno users import the npm packages through `npm:`
specifiers.

## Plan

Decided 2026-11-06 (maintainer, /quest-unblock): ✅ drop JSR. Over the
preceding 30 days `@moq/net` had about 42 JSR downloads against about 341k
on npm, and `@moq/hang` about 38 against about 306k. Not chosen: keeping JSR
behind a `JSR_TOKEN` in the `npm` Environment, which was
[read-only pull-request CI](/quest/m0/ci/read-only-secrets.md)'s step for
JSR, whose repository OIDC trust cannot require an Environment.

`main` no longer generates `jsr.json`, runs `deno publish`, or passes a
`JSR_TOKEN`; the same change is backported to `release`, which publishes.
Checked 2026-11-07: no `JSR_TOKEN` exists as a repository secret or in the
`npm` Environment, so no secret needs removing.

Archiving needs the maintainer's JSR login. Archiving also ends the
repository's OIDC link, so no separate unlinking is needed. Archive only after
the backport lands, or a `release` push can still try to publish.

Public API: none. Wire: none.

## Waiting

- moq-dev/moq: the `release` backport that drops JSR publishing merges; check `git grep -n deno origin/release -- js/common .github/workflows/release-js.yml` prints nothing
- Maintainer: archive each `@moq` package on jsr.io (agents have no JSR login, and JSR offers no archive API for tokenless callers). For each of `auth`, `binary`, `boy`, `flate`, `hang`, `json`, `loc`, `msf`, `net`, `pattern`, `publish`, `room`, `signals`, `token`, `watch`: open `https://jsr.io/@moq/<name>/settings`, set the description to `Moved to npm: https://www.npmjs.com/package/@moq/<name>`, then click "Archive package". `boy`, `publish`, and `watch` never published a version, so deleting them from the same page also works (recommended: archive all 15 for one uniform state). Check with `curl -s https://api.jsr.io/scopes/moq/packages | jq '[.items[] | select(.isArchived | not) | .name]'` printing `[]`

## Related

- [Read-only pull-request CI](/quest/m0/ci/read-only-secrets.md) - its JSR token step is replaced by this quest
- [Claim the new @moq npm names](/quest/m3/release/npm-first-publish.md) - claims only npm names once JSR is gone
