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

Open, and blocking: first-hop identity across relays. A relay advertises one
best route per prefix to each peer, and the peer pins a front to that route's
first hop (moq#3312). If the relay serves a path from a different pool member,
the peer's front names the wrong publisher, and a later failover through
another route with that first hop splices different content. The mismatch
exists today, narrowly: a front stays pinned after its prefix's best route
changes, and a NO_CAPACITY re-resolution picks another advertiser. Spreading
makes it the common case. Options:

1. Report the serving publisher per request (on TRACK_INFO or SUBSCRIBE_OK),
   and pin the downstream front to that instead of the advertised route.
   Recommended: it fixes the existing mismatch too, at the cost of a wire
   field and a lite draft change.
2. Spread only at the relay directly connected to the pool, and have it
   re-originate the prefix so downstream identity names the relay. Cheaper on
   the wire, but it discards the upstream chain loop detection relies on.
3. Accept the mismatch and document that a pool's members must serve
   interchangeable content. Simplest, but it moves a routing guarantee into
   every service's media contract.

Tests: one path always selects the same advertiser; a fixed set of many paths
spreads across advertisers rather than piling onto one (do not assert two
particular paths differ, which a correct hash may violate); and whichever
option lands, a downstream failover never splices one pool member's content
onto another's.
