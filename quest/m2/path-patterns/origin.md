# [L] Origin scopes

## Goal

`moq-net` origin handles retain literal roots while an arbitrary union of path
patterns authorizes and filters every publish, subscribe, and announcement
beneath them.

## Plan

- Replace `PathPrefixes` and prefix-only `Producer::scope`, `Consumer::scope`,
  and `allowed` with pattern unions. A root remains a literal coordinate
  transform; never join or mount at a wildcard.
- Watch origin-tree nodes at each pattern's literal head, then reapply the full
  matcher on broadcast creation, lookup, and announce fan-out. Patterns sharing
  a head share the node without sharing permission.
- Preserve exact grants through `with_root` and nested scopes using set-valued
  rebasing. Refuse an empty result rather than widening to the root.
- Migrate every origin-scope caller, including relay cluster sessions, HLS,
  stats slurps, examples, and native clients. Convert a legacy prefix `foo` to
  `foo/**` explicitly.
- Move genuine path filters such as stats exclusion onto the matcher. Leave
  literal namespace parsing, exact broadcast selection, and path construction
  as `Path` operations.
- Prove holes and suffix grants at the model layer, including concurrent
  announcements outside the grant never reaching a scoped consumer.

This is also the answer to the per-subscriber exclusion filter
[#2714](https://github.com/moq-dev/moq/issues/2714) asked for, to enforce
server-authoritative moderation (a deafened user's audio path) that a forked
client cannot bypass. A predicate over the announce stream cannot be that
boundary: announcements are prefix routes, so a filter decides about a set and
either hides paths the subscriber may use or leaks the ones it must not, and
`request_broadcast` resolves against the route table regardless. The
enforcing shape is a pattern-scoped grant on the consumer handle, narrowed at
resolution and announce fan-out alike, and changed at runtime through
[relay revalidation](/quest/m2/path-patterns/relay-auth.md). Narrowing a live
grant must also end the subscriptions it no longer covers, not only refuse new
ones. Prove the deafen case in the model-layer tests here: subscribe under a
room prefix, revalidate with a grant that excludes that audio path, and assert
the existing subscription closes and no further objects arrive.

## Closes

- [#2714](https://github.com/moq-dev/moq/issues/2714) - close this issue when the quest finishes
