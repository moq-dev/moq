# [L] Catalog references pin their broadcast instances

## Goal

Catalog, media, and data requests use the same pinned broadcast instance
in players and exporters. Each sibling stays on its first resolved
instance for the catalog instance's lifetime. Optional catalog epochs let
a reference name its intended sibling before that first resolution.

## Plan

Decided in the 2026-10-10 resume audit:

- Apply the same pinning policy to players and exporters. This replaces
  the earlier rule that a named sibling follows the newest epoch, including
  `@moq/watch`'s test that follows a republished rendition independently of
  its catalog. A sibling replacement does not silently retarget the old
  catalog's reference. A new catalog instance or explicit export stitch
  starts fresh pins; same-epoch recovery preserves identity.
- Add optional `epoch` beside every catalog broadcast reference: video,
  audio, text, JSON, and binary configs, plus the archive replay reference.
  Reuse the canonical UUIDv7 representation and validation from MoQ epochs.
  There is no new top-level catalog epoch. Self-references use the already
  pinned catalog broadcast; an explicit value must not contradict its
  known identity.
- An explicit reference epoch constrains the initial resolution and later
  requests wherever the transport can express and verify it. A known
  mismatch fails instead of falling back to the current winner. Conflicting
  identities for an already-pinned path cannot replace that pin through a
  catalog update.
- Lazy work has no anticipated epoch: leave it out of unresolved contribution
  references. Once a worker accepts a job it advertises a concrete path with
  a unique epoch. A proven Takeover preserves the same pinned instance; a
  replacement requires a new catalog instance. An epoch-bound request on an
  epoch-capable link requires a matching advertised epoch, not merely an
  epoch returned from an epochless claim.
- Omission remains compatible with existing catalogs: pin the first
  resolved instance, with no promise about an unresolved sibling's past.
- On epochless transports, explicit catalog epochs are best effort
  (maintainer choice). Keep the requested identity in the reference, use
  whatever identity the route can verify, and pin the resolved handle.
  Unknown identity alone is not a refusal. Document that this cannot prove
  the requested epoch, and never promote an unverified catalog value into
  transport evidence permitting seamless resume. Older readers can ignore
  the additive field and do not gain its guarantee.

Use the shared reference resolution paths rather than adding one rule per
media type. Mirror schemas and behavior in `rs/hang`, `js/hang`, native
readers/exporters and `@moq/watch`; carry the field through catalog producers,
conversions and bindings that expose these configs. A conversion that
cannot preserve the requested identity must report that limitation rather
than silently discard it. Coordinate with catalog rendition IDs, which
changes the neighboring track-name field but is not a prerequisite.

Test all reference-bearing sections in both languages: omitted/matching/
mismatched/malformed epochs, same and sibling paths, a replacement before
first resolution and after pinning, late rendition additions, updates that
conflict with a pin, new catalog instances, and an epochless transport with
an explicit reference. Catalog and media must never independently resolve
the same path across a replacement. Include archive replay references and
concurrent old/new consumers. Use existing unit, media and interop CI lanes;
run `just check`, `just test interop --all`, and `just drafts check`.

Public API: additive optional catalog fields and changed player sibling
following behavior. Wire: additive Hang catalog fields, no transport
framing change. Update the Hang draft, existing catalog/identity docs, and
upgrade notes inline. The [identity guide](/quest/m1/identity-guide.md)
consolidates the behavior separately. Explain both the strong epoch-aware
guarantee and the selected legacy best-effort behavior.

## Required

- [Source pin](/quest/m0/broadcast-epoch/source-pin.md) - exporters share their resolved catalog handle
- [JS consume identity](/quest/m0/broadcast-epoch/js-consume-identity.md) - JS resolves the actual serving identity

## Related

- [Retired requests](/quest/m0/broadcast-epoch/retired-requests.md) - held handles cannot emit unpinned requests after retirement
- [Catalog rendition IDs](/quest/m1/catalog-track-id.md) - the adjacent optional wire-track field
- [Catalog track identity](/quest/m1/catalog-tracks.md) - catalog updates cannot redefine a track's content
- [IETF epochs](/quest/m1/ietf-epochs.md) - verified catalog identity over negotiated IETF sessions
