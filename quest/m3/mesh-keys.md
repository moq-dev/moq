# [L] Mesh keys

## Goal

A mesh broadcast can be encrypted for its authorized subscribers, one-to-one
or fanout, and a mesh can authenticate frames with symmetric keys instead of
per-frame signatures. A publisher distributes per-broadcast keys on a
cacheable `.keys` track, wrapped to each subscriber's endpoint key, and rotates
them when a subscriber is removed.

## Plan

Parked 2026-10-25 (maintainer): encryption is out of the first mesh; identity
and authentication ship first. Research recorded in the planning PR: SFrame
and IETF secure objects leave key distribution open and give no sender
authenticity; MLS per broadcast is heavy for churning viewers; a sender-keys
style wrapped key track fans out like media. The AEAD stays moq-e2ee.
Revisit when a mesh user needs confidentiality from relays.

## Related

- [Mesh](/quest/m2/mesh/README.md) - the protection levels this completes
- [End-to-end encryption](/quest/m2/e2ee/README.md) - the AEAD and its key contract
