# [S] moq-relay drops ACME for now

## Goal

moq-relay has no `acme` feature and no rustls-acme dependency, so the migration to rind no longer waits on instant-acme 0.9. Its config and docs no longer offer ACME; a relay serves certificates from files or generates self-signed ones, as without the feature today.

## Plan

Decided by the maintainer on 2026-11-03: ACME is a recent feature nobody uses yet, so it is removed until [relay ACME on instant-acme](/quest/m3/relay-acme.md) brings it back on rind.

- Remove the `acme` feature, its rustls-acme dependency, the ACME config section, and the code behind it.
- Fix every doc and example that mentions it inline.

Public API: moq-relay's `acme` feature and ACME config fields are removed (breaking for anyone setting them; the title gets `!`). Wire: none.
