# [L] Catalog references pin their broadcast instances

## Goal

Catalog, media, and data requests use the same broadcast identity in players
and exporters. Announcement-following readers follow a referenced sibling's
Start, End, and Restart; readers without announcement following pin their
first resolution for the catalog's lifetime. Optional reference epochs
constrain the intended identity in either case.

## Plan

Decided in the 2026-10-10 resume audit:

- Apply the same identity policy to players and exporters. Per the
  2026-10-14 decision below, announcement-following readers handle a sibling
  replacement as Restart, never as seamless same-epoch resume. Readers that
  follow no announcements keep their first pin until a new catalog instance
  or explicit export stitch. Same-epoch recovery preserves identity.
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
  replacement follows Restart for announcement-following readers and requires
  a new catalog instance for other readers. An epoch-bound request on an
  epoch-capable link requires a matching advertised epoch, not merely an
  epoch returned from an epochless claim.
- Omission remains compatible with existing catalogs: bind the first
  resolved instance, then follow announcements only where the reader opts
  into that lifecycle. Make no promise about an unresolved sibling's past.
- On epochless transports, explicit catalog epochs are best effort
  (maintainer choice). Keep the requested identity in the reference, use
  whatever identity the route can verify, and pin the resolved handle.
  Unknown identity alone is not a refusal. Document that this cannot prove
  the requested epoch, and never promote an unverified catalog value into
  transport evidence permitting seamless resume. Older readers can ignore
  the additive field and do not gain its guarantee.

Decided by the maintainer, 2026-10-14 (from #5500's Codex review): readers
follow a referenced broadcast's announcements, since announcement is the
online/offline signal. The "no retarget" rule applies only to readers that
follow no announcement. An explicit epoch still constrains every resolution;
following does not authorize substituting a different requested epoch.

- `moqsrc`: Start binds the rendition, End idles its pad, and Restart
  restarts its pump.
- Exporters: a late first announcement starts the rendition and End ends
  it. Restart behavior belongs to
  [restart followers](/quest/m1/restart-followers.md).
- Share this in `moq_mux` (a `Source::follow`-style API; the shape is open)
  rather than per reader. `Source::bind` binds once today, as
  `failed_binding_does_not_retry_a_later_publisher` asserts; that changes.

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

## Related

- [Source pin](https://github.com/moq-dev/moq/pull/5252) - exporters share their resolved catalog handle
- [JS consume identity](https://github.com/moq-dev/moq/pull/5254) - JS resolves the actual serving identity
- [Retired requests](https://github.com/moq-dev/moq/pull/5294) - held handles cannot emit unpinned requests after retirement
- [Rust identity enforcement](/quest/m1/catalog-identity-rs.md) - the same for the Rust publishers
- [IETF epochs](/quest/m1/ietf-epochs.md) - verified catalog identity over negotiated IETF sessions
