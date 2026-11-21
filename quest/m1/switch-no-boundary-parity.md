# [M] A switch before any group drops the replaced copies in both languages

## Goal

A route switch that finds nothing held and nothing handed out (no boundary)
treats the copies it replaces the same way in `rs/moq-net` and `@moq/net`, so the
same route change delivers the same groups in both. The direct tests in both
languages assert that one behavior.

## Plan

Found in review of [moq-dev/moq#9276](https://github.com/moq-dev/moq/pull/9276),
which kept the split on purpose: the Rust `Reader::sync` drops the replaced
copies (`a_switch_before_any_group_drops_the_replaced_copy`), while the JS
`#sync` in `js/net/src/resume.ts` leaves them uncapped as incumbents until the
new copy delivers (`#joinIncumbent`, `#observe`; test "a switch before any group
leaves the replaced copy uncapped as the incumbent").

Recommendation: align `@moq/net` to Rust and drop them.

- The Rust shape is the one both TLA+ models check. Capping at nothing hung
  FailoverChain's Mesh sample, because a copy asked for nothing read as
  complete from one fetched group. Dropping the copies fixed that, and
  ResumeReader's `Sync` models the drop.
- With nothing held or handed out, a replaced copy has nothing to drain. What
  the old route still had in flight is the front's to keep until the new route
  delivers, as the Rust front does by swapping only once a copy delivers. The
  JS front's `route.incumbent` already covers a reader joining mid-switch.
- Check that JS's `#joinIncumbent` still covers a reader that joins before the
  new copy delivers once the switch drops its copies. Then port the Rust test
  and delete the JS incumbent test's no-boundary case.

Rejected: aligning Rust to the JS incumbent. It would reintroduce an uncapped
replaced copy that the models would have to learn, for no group the front does
not already keep.
