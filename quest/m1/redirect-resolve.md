# [XS] Strict or private Redirect::resolve

## Goal

`moq_tokio::Redirect` no longer offers a public way to turn a refused or
malformed GOAWAY redirect into "dial the current address". A caller either
gets the same refusal `Connection` acts on, or cannot call it at all.

## Plan

The drain line (https://github.com/moq-dev/moq/pull/4143) made
`Connection` end with `Error::RefusedRedirect` on a malformed or
policy-refused URI instead of redialing the peer that asked it to leave, and
made a certificate pin refuse a host change. It left the public
`Redirect::resolve` lenient to avoid a break: it falls back to the current
URL on any refusal and never sees the pin, so it answers differently from
the connection it documents.

Recommendation: make it private. Nothing outside `moq-tokio` calls it (only
its own unit tests), and the repo keeps things private until a consumer
needs them. If a consumer turns up, the alternative is returning the same
`Result<Option<Url>>` as the internal `target` does, so empty and refused stay distinct. Removing or changing a published method is a break, so this
targets `dev`; update `doc/lib/rs` if it mentions the method.

## Required

- `dev` has merged `main` since the drain line (moq-dev/moq#4132) landed
