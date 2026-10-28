# [M] Rust IETF FETCH honors its fill timeout

## Goal

Rust uses IETF FILL_TIMEOUT to bound waiting for missing FETCH objects on
drafts 18 and 22, and returns the draft's timeout-gap disposition without
hanging or discarding objects that are available. Local FETCH age
enforcement works on all versions.

## Plan

Decided in the 2026-10-10 resume audit: reuse the native parameter instead
of introducing a negotiated content-age extension. DELIVERY_TIMEOUT and
its later OBJECT_/SUBGROUP_ forms are subscription parameters, not FETCH
parameters. [FILL_TIMEOUT](https://www.ietf.org/archive/id/draft-ietf-moq-transport-22.html#section-9.20.5)
bounds the total wait for missing upstream objects; zero is cache-only.
It does not judge how old an immediately available object is. Keep exact
max-delay/max-age enforcement in the model alongside this approximation.

Today Rust decodes only FILL_TIMEOUT's presence (`fill_timeout: bool` in
`rs/moq-net/src/ietf/fetch.rs`) and refuses such requests because it cannot
emit the required gap statuses. Preserve the value and honor its semantics
before advertising or sending support.

Decided 2026-10-27 (maintainer): split by language. This Rust half starts
now. The JS half, in the cached JavaScript FETCH responder, shipped in
[moq-dev/moq#7051](https://github.com/moq-dev/moq/pull/7051). Keep the two
wire behaviors identical; JS does the following:

- One deadline per FETCH, counted from its arrival, and per fill from its
  start. Past it, a read takes only buffered objects.
- The rest of a group's slice that has not arrived becomes End of Unknown
  Range (0x10C) on draft 18 and End of Timed-Out Range (0x20C) on draft 22.
  The marker ends at the slice's last object, or at Object ID 2^62-1 when the
  group's end is unknown.
- A timed-out last group keeps the whole slice in FETCH_OK's End Location:
  {group, 2^62-1} on draft 22 and {group, 0} on draft 18.
- When nothing of the group went out, an End of Non-Existent Range first
  covers the absent groups before it.
- End of Range IDs are absolute, with no payload length
  ([moq-wg/moq-transport#1861](https://github.com/moq-wg/moq-transport/issues/1861)).
  The next object's Group ID Delta counts from the marker, as moxygen does.

JS sends no IETF FETCH, so this half also adds the Rust-requester to
JS-responder interop of these markers.

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
Use mocked time and the existing Rust and interop CI lanes; run
`just check` and `just test interop --all`.

Public API: reuse FETCH's max-delay option; no new content-age extension
configuration. Wire: support existing IETF parameters and gap statuses on
their applicable versions. Update existing interop/deviation and FETCH docs
inline, clearly distinguishing upstream wait time from content age.
