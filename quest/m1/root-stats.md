# [M] Each root's stats inside that root

## Goal

A relay can publish each root's stats inside that root: the `moq-stats`
tracks (totals, buckets, and the on-request prefix and presence tracks) for
the sessions admitted to the root, read with a grant on that root alone. A
reader in one root sees nothing of another root's sessions, paths, or
traffic, and reading them counts as no root's traffic. It is opt-in: a
relay that leaves it off keeps today's single `.stats` broadcast.

Non-goals: changing the `moq-stats` format, and any stats a
provider bills or hides from end viewers, which keep the top-level
`.stats` broadcast.

## Plan

Added 2026-11-21 (maintainer, `/quest-plan` in kixel-ai/kixel.ai). Kixel
embeds a relay per device, and each mesh is its own root
(`kixel-mesh/<id>`). A device in several meshes cannot show any mesh its
relay's stats: the one broadcast covers every root, `moq-stats` cannot filter
by path, and the relay counts a read of stats outside its top level as
traffic. Until this lands, Kixel shows stats only while a device has been in
one mesh since its daemon started
([kixel-ai/kixel.ai#163](https://github.com/kixel-ai/kixel.ai/pull/163)).
A multi-tenant relay that trusts each root's members with that root's stats
wants the same.

Sessions already record their stats under their grant's root
(`rs/moq-relay/src/auth.rs`), and roots already get a `presence.json`, so
this is mostly where they are published and who may read them.
Recommended: a configured name under each root, such as `<root>/.stats`,
reserved so a client grant cannot publish there (as the cluster reserves
`.stats/node`).

moq.pro keeps this off: it decided on 2026-11-04 that end viewers never read
a customer's stats, so its feed leaves the media namespace.
