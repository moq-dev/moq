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

## Required

- [Matcher](/quest/m2/path-patterns/matcher.md)
