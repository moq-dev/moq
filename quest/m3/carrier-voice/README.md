# Programmable carrier voice

## Goal

Determine whether MoQ should be the programmable call fabric between developer
clients, carrier gateways, and auxiliary call services. The design covers
developer-to-gateway and gateway-to-gateway calls; the first lab proves only a
developer client calling a SIP endpoint.

Stock phones remain on IMS/SIP/RTP. MoQ begins at a carrier-controlled gateway
or developer client, where publish/subscribe can make a live call available to
recorders, agents, transcription, translation, and conferencing without a
bespoke media fork for each service.

## Plan

- Use one role-based call model for both topologies. A developer client and a
  SIP/IMS gateway are authenticated call legs; internal gateway-to-gateway
  transport is the same protocol with two gateway legs, not a second design.
- Calling is an application protocol carried by ordinary
  [MOQT](https://datatracker.ietf.org/doc/draft-ietf-moq-transport/). A leg
  publishes a short-lived offer and call-state objects plus its Opus audio; the
  other leg subscribes and publishes its own state and audio.
  `PUBLISH_NAMESPACE` and `SUBSCRIBE_NAMESPACE` provide discovery only. Do not
  add call semantics to MOQT or treat namespace publication itself as ringing.
- Scope offers below an opaque line and random call id, never a raw phone
  number or caller-asserted identity. The E.164 destination is authenticated
  offer metadata, the usable source line comes from the credential, and the
  gateway is authoritative for routing and telephone state.
- Offers are live session state, not an offline inbox. Withdrawing the offer or
  losing its publisher cancels an unanswered call. Durable notifications,
  voicemail, retries after disconnect, and webhook delivery remain separate
  product surfaces.
- Keep IMS registration, native handset integration, roaming, emergency
  calling, lawful intercept, number provisioning, SMS/MMS/RCS, and carrier
  compliance outside the experiment. The SIP adapter is the boundary to that
  world.

## Quests

- [Call fabric protocol](/quest/m3/carrier-voice/protocol.md) - versioned
  namespaces, roles, state transitions, authorization, and both topologies
- [SIP call origination](/quest/m3/carrier-voice/sip-originate.md) - the shared
  SIP stack originates one outbound audio call for the lab
- [Developer-to-SIP proof](/quest/m3/carrier-voice/proof.md) - a developer
  publishes a call that reaches a SIP endpoint, with a passive second consumer
- [Carrier-voice verdict](/quest/m3/carrier-voice/verdict.md) - compare MoQ,
  RTP, and RoQ, then record the smallest justified product surface

## Related

- [SIP media stack](/quest/m3/sip-stack.md) - the telephone-network adapter
  this lab extends; the inbound-call product built on it is moq.pro
  (downstream) work
- [Room SDK](/quest/m2/room-sdk.md) - conferencing may eventually reuse its
  participant model, but is not required by this experiment
