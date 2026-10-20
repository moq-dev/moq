# [M] Design dynamic names under e2ee

## Goal

A written design, approved by the maintainer before any dynamic-name implementation, for
how an e2ee publisher answers a request for a track name it never declared.
It compares at least three options against the threat model of
[draft-lcurley-moq-e2ee](/drafts/draft-lcurley-moq-e2ee.md):

- reversible physical names, such as SIV encryption of the semantic name;
- a name-directory track;
- requests that carry an encrypted semantic name.

It ends by splitting the chosen design's implementation into follow-up
quests. This quest writes no implementation code.

## Plan

Decided with the maintainer on 2026-10-17:

- Design-only, with a design-review gate: open the PR as a draft and get the
  maintainer's approval of the choice before any implementation quest starts.
- The Rust publishers do not wait on it (maintainer, 2026-10-19): the
  broadcast wrappers merged in [#5876](https://github.com/moq-dev/moq/pull/5876),
  and [rust-publish](/quest/m2/e2ee/rust-publish.md) declares every track it
  serves. [Reopen](/quest/m2/e2ee/reopen.md) and the
  [consumer shape](/quest/m2/e2ee/consumer.md) still wait, since the chosen
  mapping can change the physical names they handle.

Background: a physical track name is a one-way HKDF of the semantic name
(the draft's Key Derivation section). Only the CDN sees physical names, and a
wrapper over `moq-net` translates them. Because the hash is one-way, a
publisher cannot map a request for an undeclared name back to its semantic
name, so #5876's `reserve_track` covers only names declared in advance.
`moq-net` serves undeclared names through `broadcast::Producer::dynamic()`.

For each option, weigh what a relay learns (name length, linkability across
epochs and subscribers, request patterns), whether identical requests still
share one physical name so relays dedupe and cache them, the effect on key
derivation and the nonce rules, wire and draft changes, the Rust and JS API
shape, and cost. Record the chosen design, with the rejected options and why,
where it will live: the e2ee draft if it changes the format, otherwise the
epic README.
