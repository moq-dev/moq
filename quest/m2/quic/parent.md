# [S] Establish the noq relationship

## Goal

MoQ's transport work has a named path into noq: the maintainers know what MoQ
intends to propose, each proposal has a reviewer, noq releases and security
advisories reach this repo on a known cadence, and the conditions under which
a moq-dev fork would be created are written down before one is needed.

## Plan

noq is the parent. It is already the default backend for `moq-tokio` and
`moq-uring` on `dev`, and MoQ has merged and open pull requests there. There
is no bakeoff left to run; the remaining work is coordination.

Open a tracking issue or discussion in n0-computer/noq listing the features
this questline needs, in the order MoQ will propose them: per-stream
acknowledgment progress, `RESET_STREAM_AT`, hierarchical send groups, and the
stream state machine accessors qmux needs. Record the maintainers' response
here: what they want upstream, what they would rather see as an extension
crate, and what they decline. Anything declined is the fork's charter.

Agree on a sync procedure: which noq releases MoQ tracks, how an advisory
against noq or Quinn is triaged here, and who rebases MoQ's open noq branches.
Write it into this repository's contributing docs so it survives the quest.

Do not create a fork in this quest. If a later quest's change is rejected,
that quest creates the moq-dev fork with the rejection linked, following the
[release quest](/quest/m2/quic/release.md) rules for package identity.

## Related

- [Multipath spike](/quest/m3/multipath-spike.md) - noq's multipath support is
  one reason the relationship is worth investing in
