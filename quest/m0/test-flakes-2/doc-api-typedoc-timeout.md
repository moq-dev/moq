# [S] The doc API's failing-render test holds up under load

## Goal

`doc/.vitepress/api/index.test.ts`'s "an unexported declaration that a
public one names fails the run" passes on a loaded host, and its
`test.skip` is removed.

## Plan

Planned 2026-11-10 under the repository's flaky-test rule, while iterating
[moq-dev/moq#8457](https://github.com/moq-dev/moq/pull/8457): one loaded
`just check` failed it as `this test timed out after 5000ms`, and the run
before passed. [moq-dev/moq#8426](https://github.com/moq-dev/moq/pull/8426)
saw the same timeout. It is the only case that renders inside its test body:
the other cases render at module load, which bun does not time out, so each
run of TypeDoc there races bun's default 5 second test timeout. Render it at
module load as the others do (capturing the rejection), or otherwise take
TypeDoc's run out of the timed body, rather than raising the timeout.

## Related

- [Interop's go -> cpp cell holds up under load](/quest/m0/test-flakes-2/interop-go-cpp.md) - quarantined from the same PR
