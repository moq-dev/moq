# [S] The runner's Rust client stops stamping every object

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

The cut also breaks the client's source, found porting it to `main` in the
m1 interop audit: a dynamic claim answers through `Request::prepare` and
`Prepared::publish` under a minted epoch, a track without a timescale
refuses timestamped frames, `Subscription` takes `max_delay` instead of a
max age, announcement events are `Start`, `Update`, `Restart`, and `End`,
and `track::Producer::used` is gone. Through moq-relay, the first
subscription under `data-subgroup-basic`'s dynamic claim then fails
`unroutable`: the accepted cold-start reset that
[announce takeover](/quest/m1/announce-takeover.md) removes, so check
whether that has landed before calling it a client bug. The JS client's
announcement check also moves from `"retracted"` to `"end"` when it takes
the cut. Its `data-subgroup-basic` subscriber also stalls there: on `main`,
`follow(path)` asks SUBSCRIBE_NAMESPACE for the
tuple's own namespace, and a moq-transport relay such as imquic answers only
with namespaces beneath it, never the shorter routing prefix the publisher
claimed. 0.4.x asked for the origin's scoped prefix. Untested options: hold
an `announced(Path.Pattern.subtree(prefix))` stream while requesting, or
request blindly.
