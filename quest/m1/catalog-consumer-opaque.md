# [S] An opaque catalog consumer

## Goal

`moq_mux::catalog::Consumer` is an opaque struct: callers can't match or
construct its encoding variants or its closed state from outside the crate.
Today it is a `#[non_exhaustive]` enum whose doc-hidden `Hang`, `Msf` and
`Closed` variants are still public.

## Plan

Raised by Codex on [#5561](https://github.com/moq-dev/moq/pull/5561), which
added the doc-hidden `Closed` state for catalog EOF.

- ✅ Decided 2026-10-15 (maintainer): make it opaque; a breaking change the
  next release absorbs.
- Benchmarks and tests that build a variant directly use `Consumer::new` or a
  `From<hang::Consumer>` impl instead. Expose only what a caller needs today,
  and question whether `msf::Consumer::track` must stay public.

Public API: breaking; the enum's variants become private. Wire: none.
