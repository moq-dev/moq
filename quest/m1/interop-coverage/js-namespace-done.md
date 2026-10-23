# [XS] The runner's JS client withdraws namespaces by reset

## Goal

Condition: the next `@moq/net` patch release, 0.4.6 or later, ships from
`release` carrying [moq-dev/moq#6518](https://github.com/moq-dev/moq/pull/6518),
the JS-only `release` backport of
[moq-dev/moq#5483](https://github.com/moq-dev/moq/pull/5483). It merged as
`350dee5520` after the 0.4.5 bump
([moq-dev/moq#6501](https://github.com/moq-dev/moq/pull/6501)) had shipped
without it. Check that `npm view @moq/net version` is above 0.4.5 and that
`git log origin/release -- js/net/package.json` shows the bump to it after
`350dee5520`. Releases are cut separately; this quest bumps nothing.

Then bump `@moq/net` in the moq-dev-js client of
[englishm/moq-interop-runner](https://github.com/englishm/moq-interop-runner)
to it, so `subscribe-namespace-lifecycle` passes against Cloudflare
draft-18 and aiomoqt, and delete this quest once that PR merges.

## Plan

`@moq/net` 0.4.5 and earlier withdraw a PUBLISH_NAMESPACE on drafts 17+
with FIN plus STOP_SENDING, which draft-18 §3.3.2 does not allow. Both
relays end the namespace only on RESET_STREAM, so the subscriber never sees
NAMESPACE_DONE until the publisher's session closes. The case passed from
two processes only because the exiting publisher closed its session. `main`
has sent the reset since #5483, and the runner's client passes against both
relays when built on `main` or on #6518. The Rust client already passes, so
the maintainer kept the backport JS-only.

The interop audit's standing approval covers PRs touching the `moq-dev-*`
builds.
