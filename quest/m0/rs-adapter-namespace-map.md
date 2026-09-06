# [S] moq-net: the draft-14/15 adapter never forgets a namespace, and a duplicate overwrites it

## Goal

The Rust control stream adapter survives a duplicate `PUBLISH_NAMESPACE` and an
unresolvable withdrawal the same way the TypeScript one does, and its namespace
map shrinks when a request ends.

## Plan

`Namespaces` in `rs/moq-net/src/ietf/adapter.rs` keys the draft-14/15
namespace-scoped messages by name. Direction is already part of the key, which
covers a cluster mesh where both endpoints advertise the same namespace, but
three problems remain within one direction:

- `Namespaces::insert` is a plain `HashMap::insert`, so a second
  `PUBLISH_NAMESPACE` for a live namespace overwrites the first during
  classification, before the session refuses it. The first request's later
  `PUBLISH_NAMESPACE_DONE` then resolves to the refused request, finds no stream
  in `Shared::close`, and the announcement stays up.
- `lookup_namespace_request_id` returns `Error::NotFound`, and `classify(..)?`
  in `ControlStreamAdapter::run` propagates it, so an unresolvable withdrawal
  ends the whole session rather than being dropped.
- Nothing ever removes from either map. `Shared::close` drops the stream entry
  and leaves the namespace behind, so the maps grow for the life of the session
  and a namespace can never be re-announced once first-wins lands.

Mirror the shape [#2806](https://github.com/moq-dev/moq/issues/2806) landed in
`js/net/src/ietf/adapter.ts`: register first-wins, keep a `request_id →
namespace` map so a closing request releases only the name it owns, and drop an
unresolvable withdrawal instead of failing the session. `classify` is already
tested standalone against a `Namespaces` it is handed, so the regression tests
go there.

Only reachable on draft-14/15 against a peer that sends the duplicate;
draft-19 is negotiated by default.
