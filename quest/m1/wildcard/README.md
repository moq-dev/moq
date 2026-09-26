# [S] Wildcard advertisements

## Goal

A service claims the prefix it could serve rather than enumerating every
broadcast under it; the client library filters that claim against the
pattern interest the caller asked for, so nothing on the wire spells a
wildcard. A claim is priced at what starting the work would cost. Specificity wins first: a concrete
claim shadows a wildcard regardless of cost, and prices compete within the
same specificity tier. A terminal concrete refusal does not fall through to
a catch-all; its claim must be withdrawn. Retracting a wildcard stops new
work without shedding what is already running.

Three workloads need this, and they are the three pattern shapes. A transcode
worker today announces a standby derivative for every matching live broadcast,
so announcements scale as workers times broadcasts; claiming one service
prefix, it advertises once for the whole fleet. A chat backend
cannot enumerate at all: rooms exist independently of any broadcast, and the
subtree pattern `<pid>/chat/**` expresses them. An archive serving recordings
over FETCH wants to say "if nobody is publishing this live, I have it", which
is the catch-all `**`, a claim about every path at once.

The cost of enumerating is real even though its last measurement is stale.
[relay-memory](/quest/m1/relay-memory.md) measured one announcement at 8.8 KB
per relay plus 4.3 KB per additional route before prefix routes made a
standby route a table entry, and owns remeasuring it. Whatever the current
number, every relay that hears an announcement pays it whether or not anything
there subscribes, so "workers times broadcasts" is that number multiplied
across the fleet in resident memory.

## Plan

Decided in [#3770](https://github.com/moq-dev/moq/pull/3770): publishing is
prefix-only on every wire and patterns never leave the token or the client library.
`dynamic(prefix, route)`
advertises a prefix; a suffix or catch-all claim is expressed as the
widest prefix that covers it (`**` is the root) and the request is the
authority, so the advertise half of this questline is re-scoped to prefix
claims resolved against pattern interest. The three workloads above still
hold: the transcoder claims its service prefix
([Where derived output lives](#where-derived-output-lives)), and the archive
claims the root and refuses what it does not have.

### What already exists, and what does not

Route cost already names this case: "The original publisher seeds it with its
production cost: zero for a live publish, something large for a standby that
would have to start working (a cold transcoder)"
(`drafts/draft-lcurley-moq-lite.md`). `moq_auth::Claims.publish` and
`origin::Producer` gain versioned patterns through
[Path patterns](/quest/m1/path-patterns.md), so advertisements reuse the
same exact containment check. `Cost { warm, cold }`
(`rs/moq-net/src/model/origin.rs:426`) is the route cost since
[#2925](https://github.com/moq-dev/moq/pull/2925).

[moq#3225](https://github.com/moq-dev/moq/pull/3225) moved a long way toward
this, and #3770 settled the wire: an announcement carries a path prefix on
every protocol, and a consumer filters announced paths against its pattern
interest locally.

Request resolution exists too. `Consumer::request_broadcast` mints a front per
path (`rs/moq-net/src/model/front.rs`) that selects through `best_route`: a
local broadcast first, then the longest covering prefix, filtered by the
requester's excluded hop and ordered by `route_order`, whose hash is keyed on
the requested path so one prefix's pool shares its paths. A refusal from that
tier is final, a front resumes only onto a source whose TRACK_INFO names the
same origin (the route's first hop on wires older than lite-07), and FETCH
resolves the same way. The pattern matcher itself exists:
`moq_net::{Pattern, Patterns, Segment}` and `Path.Pattern` /
`Path.Patterns` in `js/net/src/path.ts` own the shared matching, containment,
specificity, and rebasing tokens and filters reuse.

What is genuinely missing, beyond patterns themselves, is content identity.
Announcement `Epoch` was specified into lite-06 by
[#2611](https://github.com/moq-dev/moq/pull/2611), never implemented, and
removed from the draft by #3225, which retired `draft-lcurley-moq-broadcast`
with it. [moq#3312](https://github.com/moq-dev/moq/pull/3312) restored per-path identity
from the route's first hop, reversing #3225's no-splice rule, and lite-07 moved
it to the origin a TRACK_INFO reply names, since a pool's one route labels
many origins. This questline builds its collision handling on that rather than
on a generation field.

### Decisions

- **One prefix on the wire, one pattern in the token and the filter.** An
  advertisement is a path prefix; the [path-patterns](/quest/m1/path-patterns.md)
  dialect is what tokens and the consume-side filter use, matched by the
  shared matcher, so nothing resembles a second grammar and nothing on the
  wire spells a wildcard.
- **Longest prefix wins, and its refusal is final.** `best_route` filters to
  the longest covering prefix before it compares cost, and the lite and
  cluster drafts say the same. Advertisers of one prefix form one pool that
  cost and the request hash order. A refusal from the winning tier IS the
  answer and never falls through to a shorter prefix or to another
  advertiser, so a transcoder refusing a path does not leak the request to
  the archive's catch-all, and one unserved path costs one round trip. The
  accepted consequence: an offline derivative is not reachable through the
  catch-all while a longer prefix covers it.
- **A claim is a POOL, not a competitor.** Several advertisers of one
  prefix is the normal state, not a hazard: every transcode worker claims the
  same prefix and takes a share. What distributes them is a
  deterministic hash of the REQUESTED path against each advertiser, so distinct
  paths spread rather than one advertiser winning the whole prefix.
  Distribution is the requirement, not any particular pair: a correct hash may
  legitimately rank the same advertiser first for two given paths, so what must
  hold is that a large path set spreads and that one path always resolves the
  same way. Cost orders the pool first, which keeps work local and makes a
  distant advertiser the overflow rather than an equal peer.
- **A claim is priced, not special-cased.** Within a tier, route selection
  stays one comparison on one metric. Concrete-versus-claim is not decided
  by price at all: a concrete announcement is the longest prefix, so "longest
  prefix wins" above already shadows every claim behind it at any cost. The
  accepted consequence follows from that rule's finality: a live session's
  concrete claim shadows a healthy pool even when its service is
  broken, its terminal refusal does not fall through, and the shadow lasts
  exactly as long as the claiming session that carries it.
  The seed still has a floor, because standby and running claims of equal
  specificity do meet: a standby concrete claim (`with_cost(1000)` is the
  existing per-broadcast convention) shares a tier with a running publisher's
  concrete announcement and with warm-advertise's exact-path warm routes. The
  floor MUST exceed the deployment's enforced maximum charged-link count
  times its enforced maximum link cost (32 links at cost at most 5 gives
  a bound of 160, with producing origins seeded at 0), or a nearby standby outranks a distant running copy and the
  mesh starts a second encode of a stream it is already serving. That floor
  replaces the ad-hoc standby bias the moq.pro (downstream) transcode worker
  carries today, and it is the same stride discipline
  [pop-skipping](/quest/m1/pop-skipping/README.md) states for provider
  economics.
- **A claim is a capability, not an inventory.** It advertises what the
  sender could serve, never that a given path exists. Refusal is how a specific
  path is denied. This is why an over-claiming advertisement is not a defect:
  claiming the root is legal, and answering "not that one" is the mechanism.
- **Claims are visible to subscribers.** A claim is an ordinary prefix
  announcement, so it tells a client it may subscribe beneath it, and its
  withdrawal tells the client the capability is gone. The browser player's
  gate (`js/watch`'s `#isPathAnnounced`) lists a catalog rendition under any
  covering prefix, so a lazily-produced rendition is discoverable without an
  announcement that only demand would produce.
- **Every refusal is terminal, and capacity lives in the route.** An
  advertiser resets a request it will not serve, and the relay propagates it
  without retrying another advertiser, on moq-lite and moq-transport alike,
  so the protocols need no capacity code. An advertiser sheds load by
  withdrawing or re-pricing its route before it runs out, leaving headroom
  for requests already in flight, since a withdrawal and a request for the
  slot it gave away can cross. A request that still loses the race fails,
  and the subscriber re-requests against the updated table. No negative
  cache; rate limiting stays with the advertiser and the per-project auth
  gate.
- **A double claim is settled by route identity, not by a lease.** Two relays
  can hash one path to different workers before either concrete announcement
  propagates, and both land at the SAME literal path. Whichever route wins
  selection serves it, and a consumer moves between them only when the winner's
  identity is preserved, per the resume rule (the origin a TRACK_INFO reply
  names on lite-07, the route's first hop before it); two distinct workers are two identities,
  so the loser's subscribers end and resubscribe rather than being spliced onto
  another worker's frames mid-group. Claim routing invents neither a lease
  nor a generation. This is weaker than the retired `Epoch` design, which could
  declare two workers' output interchangeable and splice between them; a service
  that needs that guarantee has to carry it in its own media contract, not in
  routing.
- **Patterns are independent of clustering.** The `moq-pattern` crate owns
  the matching semantics tokens and filters share, with no draft of its own;
  no announce message carries a pattern on either protocol (AUTH grants on
  lite-06 do, per [Path patterns](/quest/m1/path-patterns.md)). moq-cluster adds hop
  lists, costs, pool selection, and request resolution to prefix
  advertisements.

### Where derived output lives

A prefix claim needs the variable part of a path trailing, so a fleet-wide
service claims its own prefix and mirrors the source path beneath it
(`.transcode/<pid>/foo.hang`) rather than publishing beneath the source. A
suffix such as `**/transcode.pro` collapses to the root on the wire, where it
would pool with the archive's claim and a refusal from the wrong member is
final. The source's catalog reaches the contribution through a
cross-broadcast reference. The platform layout, grants, and metering are
the deployment's; moq.pro's is in its
[wildcard questline](https://github.com/moq-dev/moq.pro/blob/main/quest/m2/wildcard/README.md).

The archive serves the source path itself: a recording IS the broadcast,
served from storage through the root claim, and a live publisher's concrete
announcement shadows it. A claim names no generation, so a client that must
distinguish recording generations reads the catalog's archive entry
([archive](/quest/m1/archive/README.md)) rather than announce state.

## Related

- [path-patterns](/quest/m1/path-patterns.md) - owns the pattern dialect
  and the shared matcher tokens and filters reuse
- [archive](/quest/m1/archive/README.md) - an archive claims the root, and its
  catalog names the generations a claim cannot
- [pop-skipping](/quest/m1/pop-skipping/README.md) - it owns the route cost and
  the rank hash this reuses
- [Broadcast epochs](/quest/m1/broadcast-epoch/README.md) - derived output
  mirrors the source path, `@<epoch>` segment included
