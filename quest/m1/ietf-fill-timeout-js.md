# [M] JS IETF FETCH honors its fill timeout

## Goal

The JS IETF FETCH responder and subscriber use FILL_TIMEOUT on drafts 18
and 22 as [Rust does](/quest/m1/ietf-fill-timeout.md): it bounds the total
wait for missing upstream objects, zero is cache-only, and a timed-out range
gets the draft's timeout-gap disposition without hanging or discarding
objects that are available.

## Plan

Decided 2026-10-27 (maintainer): split out of
[IETF fill timeout](/quest/m1/ietf-fill-timeout.md), whose Rust half starts
now. This half waits on the cached
[JavaScript FETCH](/quest/m1/js-fetch.md) responder, not on the on-demand
one.

Follow the Rust half's decisions: reuse the native parameter rather than a
content-age extension, keep exact max-delay and max-age enforcement in the
model, never reset the total wait per missing object, and never let a timeout
claim the content never existed. Cover the Rust half's test cases on the JS
side on mocked time, plus Rust/JS interop in both directions if the Rust half
has landed. Run
`just check` and `just test interop --all`.

Public API: reuse FETCH's max-delay option. Wire: support the existing IETF
parameter and gap statuses on their applicable versions.

## Required

- [JavaScript FETCH](/quest/m1/js-fetch.md) - the cached JS responder this extends
