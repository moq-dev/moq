# [M] Grant enforcement reads the origin through the announcer

## Goal

Outgoing announce enforcement decides on exactly the advertisements the
publisher sends, with the token each one actually carried, on one origin
cursor. The race an OpenAI review found on #5148 then fails loud: clear the
request token, then end and re-announce the same uncovered path within one
driver turn. Today the new announcement is silently withheld. After this
change, the session closes `Unauthorized`, as it does for any other uncovered
announcement.

## Plan

Deferred from #5148 (review 4275208063, declined there as a structural
change). At #5148's head (`ed595beef`), two loops read the origin's
announcements through independent cursors. One is grant enforcement
(`enforce_grant` in `rs/moq-net/src/ietf/session.rs`), which closes the
session on an uncovered announcement unless a request token backs it. The
other is the publisher's namespace loop, which sends PUBLISH_NAMESPACE and
knows which advertisement carries a token. Enforcement runs first in each
driver turn. On End then Start of the same path, it still finds the old
advertisement's token backing by path and admits the new Start. The
publisher then withdraws the old request and filters out the tokenless new
offer. Nothing unauthorized reaches the wire, but the fail-loud contract
breaks. Every finding in this area on #5148 (the clear race in 4274905268,
this End-to-Start race) traces back to the two cursors.

Root problem: two readers of one event stream must agree on per-announcement
state, and nothing ties them together. Decide enforcement inside the
publisher's namespace loop, where each advertisement's token backing is
already bound to that advertisement's lifetime, and delete the second
cursor. Keep the contracts #5148 settled: a token stands enforcement down for
the advertisements it backs, clearing the token resumes enforcement only for
later announcements, and a live token-backed advertisement keeps standing
through reprices.

Check whether the lite publisher has the same split between its
enforcement and announce cursors. If it does, fix it the same way here.

Regression tests: the End-to-Start case above closes the session
`Unauthorized`. Keep the existing tests that #5148 added around clearing the
token (`a_token_cleared_behind_a_presented_announce_keeps_the_session` and
its neighbors) passing.

Public API: none. Wire: none.

## Required

- [Request tokens](/quest/m1/auth/request-token.md) - #5148 adds the token backing this enforcement reads
