# [L] Rust readers follow referenced broadcasts' announcements

## Goal

Native readers follow a catalog-referenced sibling's announcements the way
`@moq/watch` already does, since announcement is the online/offline signal.
A reference that names an `epoch` never follows onto another instance.

## Plan

Split from the catalog references work (#5500's Codex review, decided by the
maintainer 2026-10-14). That work landed the optional reference `epoch`, its
shared resolution in `moq_mux::Source` and `@moq/watch`, and the Hang draft
text; this quest owns the following behavior it left out.

- Share it in `moq_mux` (a `Source::follow`-style API; the shape is open)
  rather than per reader, built on `origin::Consumer::announced_broadcast`
  rather than another hand-rolled loop. Follow unpinned and check each
  handed-out instance's epoch, as `Source::bind` does: a pinned
  `announced_broadcast` waits out an epochless route, which would break the
  best effort the reference epoch promises there. `Source::bind` binds once
  today, as `failed_binding_does_not_retry_a_later_publisher` asserts; that
  changes. The "no retarget" rule applies only to readers that follow no
  announcement.
  Decided 2026-10-20 (maintainer): this quest owns `Source::follow` and
  [sink restart follow](/quest/m1/sink-restart-follow.md) reuses it, so
  no reader hand-rolls its own follow loop.
- `moqsrc`: Start binds the rendition, End idles its pad, and Restart
  restarts its pump.
- Exporters: a late first announcement starts the rendition and End ends
  it. Restart behavior belongs to
  [sink restart follow](/quest/m1/sink-restart-follow.md).
- A replacement is a Restart for an announcement follower, never a seamless
  same-epoch resume. A proven Takeover preserves the same instance.
- A reference with an `epoch` follows only its own instance: following never
  substitutes a different epoch, so a Restart onto another one ends that
  rendition, matching `relativeBroadcast` in
  `@moq/watch` (`follows a replaced sibling only for a reference that names
  no epoch`).
- `@moq/watch` returns nothing for a rendition whose followed broadcast proves
  another epoch, but still lists it in `out.catalog`, so selection can pick a
  rendition that never plays. Drop it from the effective catalog too.

Test late first announcements, End then Start, Restart with and without an
epoch, and concurrent old/new consumers, in the moq-mux unit tests and the
`moqsrc` and exporter suites.

## Related

- [Sink restart follow](/quest/m1/sink-restart-follow.md) - exporters and sinks that follow an epochless Restart of their own broadcast
- [Export rendition rebind](/quest/m1/export-rendition-rebind.md) - exporters rebuild a rendition whose reference changes
- [Hang contribution references](/quest/m2/contribution-ref.md) - builds its lazy contribution entries on these reference rules
