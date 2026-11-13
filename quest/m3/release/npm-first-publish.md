# [XS] Claim the new @moq npm names

## Goal

`@moq/clock`, `@moq/libav`, `@moq/stats`, `@moq/ui`, and `@moq/video` exist on npm, each with trusted
publishing attached to `moq-dev/moq` and the `npm` Environment. The cut's release workflow can then publish
their real versions. A maintainer does this by hand. Delete this quest once
the checks pass:

- `npm view @moq/<name>` resolves for all five, and the package settings show
  the `moq-dev/moq` trusted publisher with Environment `npm`.

## Plan

Found 2026-10-22 while planning the release audit's follow-ups: these three
names returned 404, and nearly every published package (net, hang, watch,
publish, auth, signals, and more) now depends on `@moq/clock`. npm trusted
publishing attaches only to an existing package, so without this the cut's
publish fails on these names and still publishes dependents that can't
install. [moq-dev/moq#6243](https://github.com/moq-dev/moq/pull/6243) raised
it for `@moq/libav` alone. `@moq/ui`, which `@moq/watch` and `@moq/publish`
keep external, joined the list on 2026-11-06, when the final API audit found
it still returned 404. `@moq/stats` joined on 2026-11-12 (/quest-audit):
[moq-dev/moq#8502](https://github.com/moq-dev/moq/pull/8502) added `js/stats`
at 0.1.0 with a release script, and the name returns 404. Nothing published
depends on it, so a miss fails only its own publish. All five still returned
404 on 2026-11-12.

Decided 2026-10-22 (maintainer): publish a minimal `0.0.0` placeholder
(README only) per name to claim it, then attach the trusted publisher. The
cut publishes the real `0.1.0` through the workflow with provenance, so no
real version is published by hand. `release.ts` compares against the
`latest` tag, so the placeholder does not suppress that publish.

Until the cut publishes `@moq/libav` 0.1.0, Windows `bun install` keeps
failing, since `js/libav/build.ts` there unpacks the published tarball of
the workspace version. The placeholder does not change that. Pullfrog's
review setup, which has no Nix, hits the same `@moq/libav@0.1.0 is not
published to npm yet` install failure on every run (noted 2026-11-03 from
[moq-dev/moq#7714](https://github.com/moq-dev/moq/pull/7714), e.g.
[run 41591680387](https://github.com/moq-dev/moq/actions/runs/41591680387));
it clears with the same publication.

`@moq/p2p` is out of scope: it is private with no release script, so the cut
does not publish it. [Watch opts in](/quest/m4/p2p/watch.md) owns its first
publication, including claiming its names.

Decided 2026-11-06 (maintainer, /quest-unblock): JSR is dropped (every `@moq` JSR
package was archived on 2026-11-07), so no JSR names are claimed.

Public API: none. Wire: none.

## Related

- [Dart publication](/quest/m3/release/dart-publish.md) - the same trusted-publishing check for pub.dev
