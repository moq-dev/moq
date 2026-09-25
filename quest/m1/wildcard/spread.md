# [M] Spread

## Goal

Equal-cost advertisers of one prefix share its paths: a relay spreads distinct
requested paths across the pool, and one path always resolves to the same
advertiser. A transcode pool claiming the root today sends every job to one
worker until its cost changes.

## Plan

`best_route` (`rs/moq-net/src/model/origin.rs`) orders the winning tier with
`route_order`, whose hash tie-break is keyed on the advertised prefix. Every
path under a pool's shared prefix hashes the same, so one worker takes them
all. Keying that hash on the requested path spreads them; cost still orders
first, so a distant worker stays overflow rather than an equal peer. The lite
draft's Routing tie-breaks name no hash, so spell this one there too.

Decided: stitching identity comes from the reply, not the announced route.
A relay advertises one best route per prefix to each peer, and today the peer
pins a front to that route's first hop (moq#3312). Once a relay serves a path
from a different pool member than the one it advertised, that label is wrong,
and a later failover through another route with the same first hop splices
different content. The mismatch already exists narrowly (a front stays pinned
after its prefix's best route changes; a NO_CAPACITY re-resolution picks
another advertiser), and spreading makes it the common case.

- The subscribe and fetch replies name the origin that actually serves the
  request, and a relay stitches a failover only between replies naming the
  same origin. Differing origins end the subscription and the subscriber
  re-requests. Where the field sits (SUBSCRIBE_OK, TRACK_INFO, or the fetch
  reply) is the implementer's call; it lands in lite-07 (`moq-lite-07-wip`)
  and the lite draft, and older versions keep today's first-hop pinning.
- Rejected: re-originating the prefix at the pool's relay (identity names the
  relay, but a pool membership change re-hashes under the same identity), and
  documenting that pool members must be interchangeable (independent encoders
  are not).
- Whether the hop list is needed at all once identity moves to the reply is
  the m2 plan quest added in moq#4158; this
  quest does not wait on it.

Tests: one path always selects the same advertiser; a fixed set of many paths
spreads across advertisers rather than piling onto one (do not assert two
particular paths differ, which a correct hash may violate); and whichever
option lands, a downstream failover never splices one pool member's content
onto another's.
