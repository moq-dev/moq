# [S] IETF peers without MoQ Hidden see hidden namespaces

## Goal

A moq-transport peer that did not declare the MoQ Hidden setup option is
advertised every namespace, hidden ones included. A peer that declared it
keeps today's behavior: hidden namespaces are left out unless its
SUBSCRIBE_NAMESPACE opts in. Rust and JS publishers agree, and
`drafts/draft-lcurley-moq-hidden.md` says so.

Today the rule applies whether or not the peer declared the option, so a
third-party IETF client can never see a hidden namespace except by naming its
dot segment in the prefix.

## Plan

Maintainer decisions:

- Hiding exists to keep existing moq-lite customers (lite-06 and older) from
  seeing new `.pro/` broadcasts. None of them use an IETF client, and the
  extension will not be adopted by the IETF draft, so an IETF peer that
  never heard of it gets everything. A peer that declares it chooses per
  subscription whether to filter on the wire.
- Our own IETF clients already declare the option and filter locally when a
  reader does not ask for hidden, so their behavior does not change.
- moq-lite is unchanged: lite-06 and older never see hidden paths, and lite-07
  opts in.

Guidance:

- Both publishers, both the SUBSCRIBE_NAMESPACE path and the unsolicited
  PUBLISH_NAMESPACE path, and draft-14/15 (PUBLISH_NAMESPACE requests) as well
  as 16+ (inline NAMESPACE entries).
- Rewrite the draft's "applies whether or not the peer declared the option"
  rule and keep `just drafts check` green. `doc/concept/moq-lite.md` also
  says a peer without the extension never discovers hidden routes; update it
  in the same change.
- Tests: an undeclared peer sees hidden namespaces, a declared peer without
  the parameter does not, on at least one pre-16 and one 16+ draft. Also fill
  the gaps where nothing is tested today: draft-14/15 with hidden, JS IETF
  without solicitation, and local filtering when a peer sends hidden paths the
  reader did not ask for.
- Hiddenness is measured from the requested prefix, as the draft already
  defines it and JS already does. Rust measures from the publish origin's
  scope heads instead, so a publish scope like `.stats/**` exposes
  `.stats/node` to a request for the empty prefix; bring Rust in line and test
  that case in both languages.

Public API: none. Wire: behavior change for IETF peers that did not declare
MoQ Hidden.
