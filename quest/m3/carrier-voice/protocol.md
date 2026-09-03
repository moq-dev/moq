# [M] Call fabric protocol

## Goal

A versioned experimental protocol defines how authenticated call legs offer,
accept, carry, and end an audio call over ordinary MoQ. It describes both
developer-to-gateway and gateway-to-gateway calls without exposing phone
numbers in namespaces or trusting caller-supplied identity.

## Plan

- Define a project-scoped line inbox and opaque call/leg hierarchy. The shape
  starts at `voice/lines/<line-id>/calls/<call-id>/legs/<leg-id>`; each leg
  publishes only its own broadcast, containing an Opus audio track and a
  versioned state track. The exact wire lives beside the Rust and TypeScript
  implementations, not only in this quest.
- The first caller-leg state is an offer. For outbound telephone calls it
  carries the E.164 destination and requested capabilities. Gateway-authored
  states report routing, ringing, answer, rejection, and termination. Define
  ordering, duplicate/replay handling, terminal-state behavior, cancellation
  on withdrawal, and simultaneous teardown so reconnects cannot resurrect a
  call.
- Authorization assigns roles rather than trusting path text: a caller may
  publish its leg and request use of a configured line, the carrier gateway
  validates that line and publishes authoritative telephone state, the remote
  leg publishes its media, and an auxiliary consumer receives a least-privilege
  subscription to the call. A source number is derived from the credential and
  line configuration, never accepted from offer metadata.
- Use standard namespace discovery to notice new calls under an authorized
  line prefix. Do not extend MOQT. Bound offer lifetime, state-object size,
  call count, and per-line concurrency, and specify what is observable to a
  relay even when media objects are encrypted.
- Include sequence diagrams for developer-to-SIP and gateway-to-gateway calls,
  plus a threat model covering number enumeration, caller-ID spoofing, path
  injection, unauthorized recording, replay, abandoned offers, and confused
  deputy use of a carrier trunk.
- Land shared schema/types and state-machine tests in this repository. Keep
  HTTP/webhooks out of the wire; the verdict may recommend them later as a
  durable product-control surface without changing the live media model.

## Related

- [SIP call origination](/quest/m3/carrier-voice/sip-originate.md) - adapts the
  telephone leg to the protocol
