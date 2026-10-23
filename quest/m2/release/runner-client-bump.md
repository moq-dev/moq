# [XS] The runner's Rust client stops stamping every object

## Goal

Condition: a moq-tokio release carries
[moq-dev/moq#5083](https://github.com/moq-dev/moq/pull/5083), a breaking
change on `main` that publishes only with the next cut. Check with
`cargo info moq-tokio` and that release's changelog.

Then bump the moq-dev-rs client in
[englishm/moq-interop-runner](https://github.com/englishm/moq-interop-runner)
to it, so it stops attaching a Timestamp property to every object, and delete
this quest once that PR merges.

## Plan

The released client adds the property, which third-party subscribers count as
an unexpected extension in `data-subgroup-basic`. The interop audit's standing
approval covers PRs touching the `moq-dev-*` builds.
