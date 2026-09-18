# Cluster extension -01

## Goal

`rs/moq-net` and `js/net` speak draft-lcurley-moq-cluster-01 as
`drafts/draft-lcurley-moq-cluster.md` now reads: the HOP_ID Setup Option
replaces RELAY_HOPS on an even key, and a PUBLISH_NAMESPACE reprices with
REQUEST_UPDATE instead of a repeated PUBLISH_NAMESPACE. Once both land the
revision is published to the datatracker, answering afrind's review of -00
([#3693](https://github.com/moq-dev/moq/issues/3693),
[#3695](https://github.com/moq-dev/moq/issues/3695),
[#3697](https://github.com/moq-dev/moq/issues/3697),
[#3698](https://github.com/moq-dev/moq/issues/3698)).

## Plan

The draft moved first, on main. The text-only halves of that review, one
publisher per namespace and the single per-direction cost metric, needed no
code and are already answered there. HOP_ID landed on main; what remains is
the cost update.

Nothing deployed speaks -00: cluster sessions run moq-lite-06 (see
[PoP skipping](/quest/m2/pop-skipping/README.md)), and a -00 peer meeting a
-01 peer sees an unknown Setup Option on each side, so neither negotiates and
the session runs as plain moq-transport.

The draft on dev carries pattern-extension and selection changes main does
not. The merge keeps dev's text and this revision's wire changes together;
the quest does not touch the draft again except the changelog.

## Quests

- [Cost update](/quest/m1/cluster-01/cost-update.md) - a PUBLISH_NAMESPACE reprices with REQUEST_UPDATE, and -01 is published

## Related

- [Anonymous rank](/quest/m1/anonymous-route-rank.md) - edits the same draft's identity sections
- [Warm advertise](/quest/m2/pop-skipping/warm-advertise.md) - the repricing this update path carries
