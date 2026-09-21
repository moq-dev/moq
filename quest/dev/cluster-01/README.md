# Cluster extension -01

## Goal

`rs/moq-net` and `js/net` speak draft-lcurley-moq-cluster-01 as
`drafts/draft-lcurley-moq-cluster.md` now reads: the HOP_ID Setup Option
replaces RELAY_HOPS on an even key, and a PUBLISH_NAMESPACE reprices with
REQUEST_UPDATE instead of a repeated PUBLISH_NAMESPACE. Both have landed; what
remains is publishing the revision to the datatracker, answering afrind's
review of -00
([#3693](https://github.com/moq-dev/moq/issues/3693),
[#3695](https://github.com/moq-dev/moq/issues/3695),
[#3697](https://github.com/moq-dev/moq/issues/3697),
[#3698](https://github.com/moq-dev/moq/issues/3698)).

## Plan

The draft moved first, on main. HOP_ID landed on main and the cost update on
dev, so the revision publishes from dev, where both wire changes and the
pattern-extension and selection text live together.

Nothing deployed speaks -00: cluster sessions run moq-lite-06 (see
[PoP skipping](/quest/next/pop-skipping/README.md)), and a -00 peer meeting a
-01 peer sees an unknown Setup Option on each side, so neither negotiates and
the session runs as plain moq-transport.

## Quests

- [Publish -01](/quest/dev/cluster-01/publish.md) - the revision is on the datatracker and the four review issues carry its link

## Related

- [Warm advertise](/quest/next/pop-skipping/warm-advertise.md) - the repricing this update path carries
