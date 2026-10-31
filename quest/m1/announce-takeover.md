# [XL] Announcements take over a claim's existing instances

## Goal

A worker advertises an epochless prefix to accept lazy work, then announces
an independent, more-specific route with a unique job epoch without resetting
readers already served by that same job. A different worker or replacement
job never inherits those readers. Rust and JS expose the transition as
`Takeover`, distinct from `Restart`.

## Plan

Decided in the 2026-10-10 quest audit interview: replace response-only claim
epochs with `ANNOUNCE_TAKEOVER`. Keep this shared-model and lite-07 work in
m1 and a prerequisite of the lite-07 cut, not the m0 release. moq-transport
gets no counterpart, since [moq-transport carries no epoch](/quest/m1/ietf-epoch-cut.md)
(decided 2026-10-28), so a moq-transport hop uses Start/Restart. Ordinary
Start/Restart is a supported fallback, including the accepted cold-start
reset before this lands.

- A worker advertises `transcode/` without an epoch. Demand for a path starts
  one job, which announces `transcode/path` with its own freshly minted epoch
  through `Request::prepare` then `publish`, the claim-to-route operation.
  Lazy catalog references and initial requests need no anticipated epoch.
  Identical encoder settings do not make two workers the same instance.
- TAKEOVER is START plus a predecessor Announce ID: it carries the child
  prefix, epoch and ordinary announcement routing metadata, and allocates
  its own announcement ID. The predecessor is a one-time continuity
  reference, not a lifetime dependency. Either route can end independently;
  ending the new route never silently returns its readers to the pool.
- The predecessor must be live on the same ordered announcement stream.
  A sender that has not advertised it sends START instead. An unknown or
  retired received ID is a protocol violation. No active reader is required:
  with no instance to adopt, the message simply installs the new route.
- TAKEOVER explicitly asserts continuation of the same content, not just
  common prefix ancestry. Adopt only instances actually resolved through the
  named predecessor incarnation and covered by the new prefix. Preserve
  their handles, group positions, metadata, object and negative caches,
  catalog pins and outstanding requests. Unrelated siblings are unchanged.
  A known epoch must match; a requested epoch must be verified when possible.
- Bind the publishing operation to the same live broadcast served through
  the claim. A worker restarting a job under an unchanged claim cannot use
  TAKEOVER to relabel the previous job's cached content. Use ordinary
  replacement when continuity cannot be asserted. In the model, prove that
  the predecessor relationship identifies the accepted instance, including
  old retained generations and pending answers, before fixing the final wire
  fields; do not infer it from a matching worker or configuration.
- RESTART retains an Announce ID today. TAKEOVER names its current
  incarnation in stream order, never old handles merely sharing that ID.
  The new prefix must lie within predecessor coverage and authorization.
  Announce-request clamping may make the visible prefixes equal; distinguish
  the advertisements without requiring a strictly longer encoded prefix.
- Relays translate predecessor IDs per stream using actual serving
  provenance. An aggregated prefix can serve paths through different workers.
  A takeover from A cannot adopt B's readers. Forward TAKEOVER only where
  its continuity assertion remains true; otherwise use ordinary Start/Restart.
  Initial snapshots can use START without replaying historical takeovers.
- Add an explicit `Takeover` announcement event carrying the new announcement
  and predecessor identity, mirrored in Rust, JS and exposed bindings. Use
  model identity rather than leaking stream-local wire IDs. This replaces
  expanding Update, whose existing meaning is an in-place metadata change.
  Relays preserve the relationship and applications distinguish continuity
  from replacement. A proven takeover does not renew a composed catalog.
- An epoch-bound request still requires a matching advertised epoch; response
  metadata alone does not make an epochless route eligible. Legacy best-effort
  catalog verification remains weaker and never authorizes implicit stitching.
  On older wires, translate this optimization into ordinary announcements
  and required Restart rather than changing published framing.

Decided 2026-10-27 (maintainer): this no longer waits on
[Routes and announces](/quest/m1/cluster-routing/routes.md), and needs no
relay-minted route epochs, only the job's own epoch above: an epochless
route stays ineligible for an epoch-bound request, as above. Takeover builds on
what lite-07-wip and `main` already have: stream-local Announce IDs,
ANNOUNCE_RESTART, Base compression, `Request::prepare`/`publish`, and the
claim helpers. Its ANNOUNCE_START field layout overlaps with routes (hops
become a Route ID), so whichever of the two lands second rebases onto the
other.

Implement against lite-07-wip, and reuse the routing fields and identity
model of whichever ROUTE/ANNOUNCE shape is on `main` rather than adding a
second one. Account for TAKEOVER in announcement IDs, compression bases,
snapshot counts, route filtering, duplicate-advertisement rules and
retirement. Update the lite draft and existing identity/compatibility docs in
the implementation PR, and add a takeover section to the identity and
recovery guide (`doc/concept/identity.md`), whose cold-start step and
lazy-job lifecycle it changes; the guide does not replace inline docs.

Tests with controlled time: first view before/after takeover; absent reader;
invalid/retired predecessor; same ID after Restart; two workers at one prefix;
job restart on one worker; pending TRACK/SUBSCRIBE/FETCH and late groups across
takeover; conflicting epochs; equal clamped prefixes; independently ending
parent/child; late snapshots; multi-hop and diamond relays; mixed versions;
and held catalog/media pins. Include the actual players and composer-facing
event, not only route-table checks. Measure route/readers fanout on both axes.
Remove the cold-start warm-up reads that discard a claim's first subscription
(`claimed` in `rs/moq-net-tests/tests/integration/restart.rs` and the
`rs/moq-net/tests/integration/drained_claim.rs` helper) and assert that first subscription succeeds instead.
Run `just check`, `just test interop --all`, and `just drafts check`; wire
regressions and benchmarks into existing CI lanes.

Public API: a `Takeover` event, mirrored across languages and exposed
bindings, on top of `Request::prepare` and `publish`. Wire: lite-07-wip gains
ANNOUNCE_TAKEOVER; published versions retain their framing.

## Related

- [Claim answer epochs](https://github.com/moq-dev/moq/pull/5678) - `Request::prepare` then `publish` answers a claim at the exact path under a fresh epoch, the operation TAKEOVER continues
- [Routes and announces](/quest/m1/cluster-routing/routes.md) - the route layer whose ANNOUNCE_START layout overlaps this one's
- [Retired requests](https://github.com/moq-dev/moq/pull/5294) - replacements never substitute content into held handles
