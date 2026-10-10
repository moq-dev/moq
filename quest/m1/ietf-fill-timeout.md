# [L] IETF FETCH honors its fill timeout

## Goal

Rust and JS use IETF FILL_TIMEOUT to bound waiting for missing FETCH
objects on drafts 18-22, and return the draft's timeout-gap disposition
without hanging or discarding objects that are available. Drafts 14-17
keep their existing wire; local FETCH age enforcement works on all versions.

## Plan

Decided in the 2026-10-10 resume audit: reuse the native parameter instead
of introducing a negotiated content-age extension. DELIVERY_TIMEOUT and
its later OBJECT_/SUBGROUP_ forms are subscription parameters, not FETCH
parameters. [FILL_TIMEOUT](https://www.ietf.org/archive/id/draft-ietf-moq-transport-22.html#section-9.20.5)
bounds the total wait for missing upstream objects; zero is cache-only.
It does not judge how old an immediately available object is. Keep exact
max-delay/max-age enforcement in the model alongside this approximation.

Today Rust decodes only FILL_TIMEOUT's presence and refuses such requests
because it cannot emit the required gap statuses. Preserve the value and
honor its semantics before advertising or sending support. JS's IETF FETCH
responder comes from its existing quest; build on that surface.

Propagate an explicit FETCH max-delay as the upstream wait budget where
the draft supports it. For omitted max-delay, retain the wire's omitted
FILL_TIMEOUT behavior; publisher retention still applies locally. A zero
mapped budget means cache-only on this wire, an accepted difference from
a pure content-age test. Do not reset the total wait on every missing
object or group. Shared upstream work retains each caller's own deadline
and cancellation, as it retains each age budget.

Implement each draft's correct timed-out/unknown range status and the
receiving side's terminal disposition; a timeout must not become a claim
that the content never existed. Do not wait forever for the timed-out
range, reset the entire session, or silently reinterpret FILL_TIMEOUT as
publisher retention. Test standalone and applicable joining/fill forms,
reordered arrivals, cache-only requests, partial availability, no subsequent
writes, cancellation, shared callers, and an older peer without this field.
Use mocked time and the existing Rust/JS and interop CI lanes; run
`just check` and `just test interop --all`.

Public API: reuse FETCH's max-delay option; no new content-age extension
configuration. Wire: support existing IETF parameters and gap statuses on
their applicable versions. Update existing interop/deviation and FETCH docs
inline, clearly distinguishing upstream wait time from content age.

## Required

- [FETCH max-delay](/quest/m1/fetch-max-delay.md) - per-caller age budgets and their public option
- [JavaScript FETCH](/quest/m1/js-fetch.md) - the JS responder this extends

## Related

- [moq-transport ranges](/quest/m1/subscribe-ranges/ietf.md) - sparse range forwarding and gap handling
