# [L] Origin scopes

## Goal

`moq-net` origin handles retain literal roots while an arbitrary union of path
patterns authorizes and filters every publish, subscribe, and announcement
beneath them, and an announcement reports how it matched the scope. The
announce event shape is a published API in every language, so it settles on
dev before the release.

## Plan

- Extend the pattern-valued scope API to every supported pattern union,
  removing its explicit refusal of non-prefix grants without changing its public
  signatures: Rust `scope(&Patterns)`, JS `announced(scope: Path.Pattern)`. A
  root remains a literal coordinate transform; never join or mount at a
  wildcard.
- Announce events carry a match, not a bare pattern: the covered pattern
  relative to the origin, plus one capture per wildcard in the scope, the way a
  regex match exposes the whole match and then its groups. `foo/*/chat`
  matched by `foo/alice/chat` captures `alice`; `foo/**` matched by
  `foo/alice/chat` captures `alice/chat`. Decide how a claim that is itself a
  pattern reports a capture the scope cannot pin (a `**` route under a `*`
  scope). The wire keeps echoing only the suffix beneath the literal head; the
  match is assembled locally. Mirror the shape in Rust, JS, and every binding
  that exposes announcements (`moq_announced`, the wrappers).
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
resolution and announce fan-out alike, and changed at runtime when the
lease the relay holds (`moq_auth::lease`) is revalidated. Narrowing a live
grant must also end the subscriptions it no longer covers, not only refuse new
ones. Prove the deafen case in the model-layer tests here: subscribe under a
room prefix, revalidate with a grant that excludes that audio path, and assert
the existing subscription closes and no further objects arrive.

## Closes

- [#2714](https://github.com/moq-dev/moq/issues/2714) - close this issue when the quest finishes

## Related

- [Pattern interest](/quest/m2/path-patterns/interest.md) - carries the same scopes on the lite-06 wire once they are enforced here
