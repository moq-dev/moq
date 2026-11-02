# [S] A relay says why a relayed track ended

## Goal

A relayed track that ends because the relay refused it, for example a malformed
track, reaches subscribers with an error that names why. Today it is a
PUBLISH_DONE `INTERNAL_ERROR`, so bindings only see `Internal`. The baseline
harness also keeps the publisher's and consumer's page console logs.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-complete run). Follow-ups from [moq-dev/moq#7135](https://github.com/moq-dev/moq/pull/7135).
Pick the most specific code each wire version carries, and document the mapping
in `doc/concept/standard.md`.
