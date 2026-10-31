# [M] Tokens stay out of logs on release

## Goal

No credential reaches a log line at any level on `release`: SETUP and AUTH
tokens, `?jwt=` queries in dialed, accepted, or GOAWAY redirect URLs, and
`Cookie`/`Authorization` headers. Refusal logs escape client-controlled
fields, so a path or root cannot forge a log record.

## Plan

[moq-dev/moq#7200](https://github.com/moq-dev/moq/pull/7200) fixed this on
`main` by giving each existing credential-bearing type a manual redacting
`Debug` (SETUP and AUTH tokens, opaque SETUP parameters, lite request targets,
CONNECT requests, auth requests, cluster peers, GOAWAY URIs) and escaping
refusal fields, with captured-trace tests for each.

Port that behavior in a separate PR against `release`, preserving its
published API, per `CONTRIBUTING.md`. Its protocol and transport layouts
differ from main, so port behavior rather than applying the patch blindly.
Known leaks there: the SETUP token derives `Debug`, the auth refusal log is
unescaped, and all three GOAWAY types derive `Debug` while
`lite/publisher.rs` logs the redirect URI raw.

Settled: ✅ keep the existing credential types with manual redacting `Debug`,
rather than one shared credential type, so moq-net and moq-auth keep their
independent public types and byte fields.
