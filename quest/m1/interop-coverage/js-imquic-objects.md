# [XS] The runner's JS client writes data stream headers in one write

## Goal

Condition: the next `@moq/net` patch release, 0.4.7 or later, ships from
`release` carrying [moq-dev/moq#6769](https://github.com/moq-dev/moq/pull/6769),
the JS-only `release` backport of
[moq-dev/moq#6504](https://github.com/moq-dev/moq/pull/6504). Check that
`npm view @moq/net version` is above 0.4.6 and that
`git log origin/release -- js/net/package.json` shows a bump after the
backport merged. Releases are cut separately; this quest bumps nothing.

Then bump `@moq/net` in the moq-dev-js client of
[englishm/moq-interop-runner](https://github.com/englishm/moq-interop-runner)
to it, so `data-subgroup-basic` passes against imquic (draft-22,
WebTransport), and delete this quest once that PR merges. If imquic fixes
[meetecho/imquic#43](https://github.com/meetecho/imquic/issues/43) first and
the runner rebuilds its image, the cell turns green without the bump; delete
the quest then.

## Plan

imquic drops a SUBGROUP_HEADER or FETCH_HEADER that arrives across more
than one stream read, so the relay discards every object and logs
`No track found for that subscription` with alias 0 at the stream's FIN.
`@moq/net` 0.4.6 and earlier write the header one field per write, which
the polyfill sends as separate frames. `main` writes it in one write since
#6504, and the backport does the same on `release`. Against the runner's
imquic relay image, the runner's client fails every run on 0.4.4 and passes
3/3 on the backport branch.

The runner's client must stay on the 0.4 line for this: `main`'s announced
request watches the tuple's exact path, which moq-transport relays answer
only with namespaces beneath it, so it never discovers the shorter routing
prefix the publisher claimed. That is recorded on
[Runner client bump](/quest/m2/release/runner-client-bump.md), which moves
the client to `main`'s cut.

The interop audit's standing approval covers PRs touching the `moq-dev-*`
builds.
