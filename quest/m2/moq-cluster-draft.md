# [M] The moq-cluster draft says what ships

## Goal

`drafts/draft-lcurley-moq-cluster.md` specifies what moq-net's moq-transport
binding (`rs/moq-net/src/ietf/cluster.rs`, `js/net/src/ietf/cluster.ts`)
sends and accepts, and names the third-party relays that negotiate it.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): the
moq-cluster draft was to be retired with its moq-transport binding
(rejected: reconciling the draft; an "out of date" note), unless a
third-party moq-transport relay turned out to negotiate the extension, in
which case the draft is rewritten to what ships and says who.

Found 2026-11-17 by [moq-dev/moq#9053](https://github.com/moq-dev/moq/pull/9053),
which kept the binding: two do.

- [openmoq/moqx](https://github.com/openmoq/moqx) (C++, on moxygen)
  implements draft-lcurley-moq-cluster-01 and negotiates it by default
  (`docs/moq-cluster.md`, `src/relay/MoqClusterRouteCodec.cpp`).
- [openmoq/moq5](https://github.com/openmoq/moq5) (C) implements -01 on its
  draft-22 session engine (`docs/moq-cluster-01-design.md`,
  `core/include/moq/cluster.h`).

Decided 2026-11-18 (maintainer, on
[moq-dev/moq#9053](https://github.com/moq-dev/moq/pull/9053#issuecomment-6975349236)):
✅ the draft and its binding are kept and the draft rewritten to what ships
(rejected: retiring both anyway).

The work:

- Audit the draft against the binding, rule by rule, and rewrite it to what
  the code does. The pre-cut audit found it still specifies a warm discount
  ("Accumulating Cost"): check whether the binding sends one.
- Our draft is moq-cluster-02-wip, and both third parties implement -01.
  Its changelog lists what moved since -01 (64-bit Hop IDs, REQUEST_UPDATE
  for updates, PUBLISH carrying HOP_PATH and ROUTE_COST). Check each against
  a -01 peer, and keep the binding interoperable with -01 where it costs
  little.
- Name moqx and moq5 in the draft's introduction or an implementation
  status section.
- Add a moqx peer to `just test interop` if it builds in the dev shell, so
  the binding is checked against a third party rather than only itself.

Public API: none. Wire: none, unless the audit finds the binding departs from
-01 in a way a -01 peer cannot read.
