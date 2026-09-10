# [M] Catalog track identity

## Goal

Choose how a catalog describes a track over its lifetime so live playback and
recorded playback do not guess which configuration applies to a media group.
Explore immutable track definitions as an alternative to correlating catalog
updates with groups. This is independent of DVR and does not gate archives.

## Plan

There is no explicit catalog-update-to-group binding. Timestamps alone do not
establish one. Audit publishers, consumers, and catalog composition in Rust and
JS to identify which track properties change today and why: codec/config bytes,
resolution, audio layout, rendition metadata, and broadcast references.

Compare two approaches with concrete publish and playback examples:

- Make each track definition immutable for its identity. A configuration change
  creates a new track identity instead of changing the meaning of existing
  groups. This is the preferred direction to investigate, not a settled API.
- Keep mutable definitions with an explicit configuration identity or update
  boundary that groups can reference. Measure the protocol and lifecycle cost
  against immutable definitions rather than assuming version binding is needed.

Distinguish immutable track definitions from a completely immutable catalog.
Decide whether catalogs may add/remove tracks or change presentation metadata,
what happens to removed track identities, and whether a name can be reused.
Cover codec changes, rendition switches, reconnects, late joiners, reordered
catalog/media delivery, and readers seeking older groups. Evaluate what catalog
state must remain discoverable when media outlives the publishing session;
retaining snapshots alone cannot establish which configuration a group uses.

Present the recommendation and migration costs for maintainer agreement before
changing public APIs or wire semantics. Produce focused implementation quests
for the chosen design, including Rust/JS and binding synchronization, HLS/watch
behavior, draft changes, and CI regression coverage. Target any published API
break at dev. Do not add an archive-only version index or change DVR retention
as a substitute for deciding track identity.

## Related

- [Archive](/quest/m1/archive/README.md) - storage and replay consume the eventual identity contract
