# [L] Broadcast epoch

## Goal

Every broadcast carries a generation epoch, so a restarted publisher is never
confused with its previous run. Consumers of that identity are separate quests;
this one is the identity itself.

## Plan

[#2610](https://github.com/moq-dev/moq/issues/2610) is an open proposal, not
landed work: no epoch field exists on the wire today, and this quest is what
changes that. It puts the identity on the WIRE rather than in the Hang catalog,
which is the whole reason this is its own quest:
`ANNOUNCE_START`/`ANNOUNCE_UPDATE` gain `Epoch` and `Ended`, `SUBSCRIBE`, `FETCH`
and `TRACK` gain `Epoch` (0 = current, mismatch = reset), and `TRACK_INFO` returns
the RESOLVED epoch so a consumer can key by generation without racing a
replacement.

It replaces the first-hop route identity that stands in for content identity
today, which is why a restarting publisher with a stable origin id gets falsely
spliced. The resolution rules are the load-bearing part, because a consumer's
behaviour has to change with them: epoch is publisher-minted and strictly
increasing, forwarded unchanged, and **0 means unspecified**. Non-zero outranks 0,
higher wins over lower, equal non-zero epochs splice, and zero-vs-zero falls back
to today's first-entry identity. Receivers keep no high-water mark, which bounds
the damage of a bad epoch to its advertisement's lifetime.

That 0 is why consumers cannot assume identity exists: a publisher that mints none
resolves to 0, and a consumer keying on it must keep its old behaviour rather than
treat 0 as a generation.

`Ended` rides along and is in scope: it splits live from complete content (ended
broadcasts reject SUBSCRIBE and are read via FETCH), and is only announced to
streams whose `ANNOUNCE_REQUEST` opted in. Scope includes the IETF side, where
EPOCH is a negotiated moq-transport parameter, plus the bindings.

That is a moq-lite-06 protocol change spanning the model, the wire codecs, and the
JS mirrors, so land it on its own terms rather than as a side effect of a
consumer's needs. Two consumers wait on it: cacheable HLS media URLs
([HLS generation](/quest/m2/hls-generation.md)), and recording keys (#2610
proposes storing recordings at `<broadcast>/<epoch>` instead of a UUID plus an
API lookup).

## Closes

- [#2610](https://github.com/moq-dev/moq/issues/2610) - close this issue when the quest finishes
