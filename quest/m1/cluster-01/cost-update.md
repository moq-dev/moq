# [M] Cost update

## Goal

A relay that starts or stops carrying a namespace it advertised with
PUBLISH_NAMESPACE reprices it with REQUEST_UPDATE on that request stream,
carrying ROUTE_COST and, when the route changed, HOP_PATH. The receiver
answers REQUEST_OK, or REQUEST_ERROR and closes the stream, which withdraws
the advertisement. A repeated PUBLISH_NAMESPACE on a live stream is the base
draft's duplicate again. A NAMESPACE on a SUBSCRIBE_NAMESPACE stream is still
re-sent to update it. With both wire changes in,
draft-lcurley-moq-cluster-01 is on the datatracker.

## Plan

Today `sync_namespace` in `rs/moq-net/src/ietf/publisher.rs` re-sends
PUBLISH_NAMESPACE on the stream that carries it, and
`run_publish_namespace_updates` in `rs/moq-net/src/ietf/subscriber.rs`
decodes the repeat as an update.

- Sender: a repricing writes REQUEST_UPDATE (type 0x2, Request ID plus
  parameters) with the changed parameters. REQUEST_UPDATE keeps an omitted
  parameter, so a change to 0 sends ROUTE_COST as an explicit 0, unlike the
  initial advertisement where absent means 0. Reuse the REQUEST_UPDATE codec
  the subscribe path has in `rs/moq-net/src/ietf/subscribe.rs`, extended to
  carry HOP_PATH and ROUTE_COST. MAX_REQUEST_UPDATES bounds the outstanding
  updates per stream, so wait for REQUEST_OK before the next one or coalesce
  to the latest value.
- Receiver: `run_publish_namespace_updates` accepts REQUEST_UPDATE, merges
  the parameters onto the held advertisement, and replies REQUEST_OK. A
  repeated PUBLISH_NAMESPACE on the stream is a duplicate. An update it
  cannot apply gets REQUEST_ERROR and a closed stream, per moq-transport
  Section 9.5.1, which withdraws the advertisement.
- Draft-17 through draft-21 all allow REQUEST_UPDATE on PUBLISH_NAMESPACE,
  which is every version the extension negotiates on, so no per-version
  branch.
- js/net is a leaf that seeds cost 0 and never reprices, so it needs no
  sender. Where it reads a repeated PUBLISH_NAMESPACE as an update today, it
  reads REQUEST_UPDATE instead and treats the repeat as the duplicate.
- Branch from dev: the announce loops in `publisher.rs` and `subscriber.rs`
  were rewritten there and m1 is the dev line. Main keeps the repeat until
  the merge.
- Tests: a repricing round trip over REQUEST_UPDATE, including an explicit
  0; a repeated PUBLISH_NAMESPACE on its own stream is refused; a failed
  update withdraws the advertisement; a NAMESPACE re-send still updates.
- Publish: `just drafts publish draft-lcurley-moq-cluster 01 <email>`, then
  the maintainer clicks the confirmation link. Reply on the four issues with
  the -01 link.

## Required

- [HOP_ID option](/quest/m1/cluster-01/hop-id.md) - -01 publishes with both wire changes in

## Closes

- [#3698](https://github.com/moq-dev/moq/issues/3698) - close this issue when the quest finishes
