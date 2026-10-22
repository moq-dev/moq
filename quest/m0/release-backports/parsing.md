# [S] Backport bounded JS certificate and path parsing

## Goal

Release's JS connection code rejects malformed certificates and paths without
quadratic PEM scanning or allocating one string for every path separator.
Valid inputs retain the published behavior and API.

## Plan

Decided 2026-10-22 (maintainer): port the JS input-hardening portion of
[moq-dev/moq#6084](https://github.com/moq-dev/moq/pull/6084) as a focused
backport. Its publishing changes already protect workflow-run events from
main and need no release backport.

Release 36f392f492 still uses a lazy certificate-armor regex and calls
parts(path).length before rejecting excess path components. Repeated BEGIN
markers without an END cause repeated scans, while slash-heavy paths create
an unnecessary allocation for every part.

Reproduce both costs on release, then port the linear delimiter search and
early separator count with their edge-case coverage. Keep the existing path
limit and PEM acceptance contract. Measure increasing malformed input sizes
with the repository's benchmark conventions so parsing growth remains
visible; unit tests should assert behavior, not elapsed time. The consumers
are certificate-hash connection setup and outgoing path encoding in
@moq/net. Public API and wire formats are unchanged.
