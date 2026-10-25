# [XS] Claim the new @moq npm and JSR names

## Goal

`@moq/clock`, `@moq/libav`, `@moq/video`, and `@moq/p2p` exist on npm, and
the three with JSR packages exist on JSR, each with trusted publishing
attached to `moq-dev/moq`. The cut's release workflow can then publish
their real versions. A maintainer does this by hand. Delete this quest once
the checks pass:

- `npm view @moq/<name>` resolves for all four, and the package settings show
  the `moq-dev/moq` trusted publisher.
- `https://api.jsr.io/scopes/moq/packages/<name>` returns 200 for `clock`,
  `video`, and `p2p`, each linked to the repository. `@moq/libav` sets
  `"jsr": false`.

## Plan

Found 2026-10-22 while planning the release audit's follow-ups: all four
names return 404, and nearly every published package (net, hang, watch,
publish, auth, signals, and more) now depends on `@moq/clock`. npm trusted
publishing attaches only to an existing package, so without this the cut's
publish fails on these names and still publishes dependents that can't
install. [moq-dev/moq#6243](https://github.com/moq-dev/moq/pull/6243) raised
it for `@moq/libav` alone.

Decided 2026-10-22 (maintainer): publish a minimal `0.0.0` placeholder
(README only) per name to claim it, then attach the trusted publisher. The
cut publishes the real `0.1.0` through the workflow with provenance, so no
real version is published by hand. `release.ts` compares against the
`latest` tag, so the placeholder does not suppress that publish.

Until the cut publishes `@moq/libav` 0.1.0, Windows `bun install` keeps
failing, since `js/libav/build.ts` there unpacks the published tarball of
the workspace version. The placeholder does not change that.

Public API: none. Wire: none.

## Related

- [Dart publication](/quest/m2/release/dart-publish.md) - the same trusted-publishing check for pub.dev
