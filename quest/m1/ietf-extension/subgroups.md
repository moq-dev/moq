# [L] moq-transport subgroups through a relay

## Goal

A group can hold several moq-transport subgroups. Through moq-relay, each
subgroup reaches a moq-transport subscriber on its own stream, with its
subgroup ID, its exact publisher priority, and its FIRST_OBJECT and
END_OF_GROUP header bits. Plain readers see subgroup 0 only.

## Plan

Decided 2026-10-13 (see the [epic](/quest/m1/ietf-extension/README.md)).

- `group::Producer` and `group::Consumer` own crate-private subgroups. Subgroup
  0 is the public frame list. Every other subgroup keeps its own frames with
  their `skip`, properties, end status, and priority.
- Priority is stored as the wire's `Option<u8>`. `None` means the header
  omitted it and inherits the track default. It round-trips exactly, so 200
  stays 200 and never goes through the 128-to-127 track mapping.
- Each subgroup is served on its own downstream stream, scheduled by its own
  priority, and two subgroups never share a stream. Egress picks the most
  compact subgroup ID encoding. It sets END_OF_GROUP only on the subgroup
  carrying the group's last object.
- Duplicate-stream detection is keyed by (group, subgroup). Every subgroup
  counts toward the group's size limit.
- Plain readers see subgroup 0 only. moq-test mode 1 (subgroup ID equal to
  object ID) therefore reads as object 0 alone through lite. That is
  accepted.
- FETCH of a group with two or more subgroups gets `NOT_SUPPORTED`.
- Docs inline in `doc/concept/standard.md`: subgroups are forwarded between
  moq-transport peers, plain moq-lite readers get subgroup 0, and FETCH
  refuses multi-subgroup groups.

Tests: moq-test modes 1 and 2 shapes through the relay, including the
priority split of 200 and 201 and the per-subgroup END_OF_GROUP bit. Add a
benchmark swept over subgroups per group and subscribers, since subgroups fan
out.

Public API: none. Wire: none new.

## Required

- [Object fields](/quest/m1/ietf-extension/objects.md) - subgroups store the per-object skip, properties, and end status it adds
