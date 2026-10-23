# [M] IETF lazy discovery in JS

## Goal

A JS moq-transport session never holds two overlapping SUBSCRIBE_NAMESPACE
requests at a peer that did not declare MoQ Solicit, so a peer that enforces
the drafts' PREFIX_OVERLAP rule, such as moxygen, never refuses a broader
watch and turns the session blind.

## Plan

Rust landed in [moq-dev/moq#6530](https://github.com/moq-dev/moq/pull/6530):
lazy SUBSCRIBE_NAMESPACE on every draft, UNSUBSCRIBE_NAMESPACE on drafts
14/15 (named by prefix on 14, fixed in both languages), and exclusive
discovery at a peer that declared no Solicit (`Discovery::exclusive`,
`rs/moq-net/src/ietf/subscriber/namespaces.rs`). JS
`connection/forward.ts` already asks only about watched prefixes, but holds a
narrower request open under a broader one on every version.

- At a peer that declared no Solicit, a broader watch closes the narrower
  requests beneath it, and its SUBSCRIBE_NAMESPACE goes out only once each
  has left the wire: UNSUBSCRIBE_NAMESPACE written on drafts 14/15, the peer's
  half closed from draft 16.
- `forward.ts` needs the session to say when a request is off the wire; the
  announced consumer reports only its local close today.

Settled in [moq-dev/moq#6530](https://github.com/moq-dev/moq/pull/6530):

- JS non-overlap is this quest, not that PR: it needs the session signal
  above, and a refused JS watch only downgrades the session to blind answers.
- Overlap policy is keyed on MoQ Solicit. Requests are exclusive only at a
  peer that declared no Solicit, since only foreign peers enforce
  PREFIX_OVERLAP; our own peers keep overlapping hold semantics, because on
  drafts 14/15 our publisher withdraws what an unsubscribed request
  advertised, so exclusive-everywhere would restart routes mid-handover.
- A PUBLISH_NAMESPACE that outlives an UNSUBSCRIBE_NAMESPACE (drafts 14/15),
  or arrives unsolicited, is kept until the peer withdraws it. It is bounded
  by request credit or streams and the `announces` cap, and cancelling it
  would lose it for good, since the peer does not repeat it.

Public API: none expected. Wire: none.
