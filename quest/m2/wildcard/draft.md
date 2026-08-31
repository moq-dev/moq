# [M] Draft

## Goal

Specify the wildcard advertisement in moq-lite and mirror it in the moq-cluster
extension.

## Plan

Add the message to `drafts/draft-lcurley-moq-lite.md` under moq-lite-06, beside
ANNOUNCE_START, and mirror it in `drafts/draft-lcurley-moq-cluster.md` the way
route cost appears in both.

What the text has to settle:

- The message itself: a path pattern in its string form, the hop list, and ONE
  cost varint, retracted and replaced through the same id-referencing forms
  ANNOUNCE_END and ANNOUNCE_UPDATE already use. The pattern is the
  [path-patterns](/quest/m2/path-patterns/README.md) dialect: segments that
  are literal, `*`, or `lit*lit` (one in-segment star), plus at most one `**`;
  a star-free pattern is exact and a subtree is written `foo/**`. Define it
  normatively in the
  draft, citing Ant-style path patterns as the convention (`*` within a
  segment, `**` across segments) and noting MQTT topic filters as the pub/sub
  precedent that lacks the suffix and in-segment forms this needs. The lite
  draft currently says prefix matching is "byte-by-byte" while the
  implementation is segment-aware; align the text on segment-aware while
  adding the pattern, rather than speccing a rule the code contradicts. One cost
  varint rather than `Cost`'s warm/cold pair: the halves
  differ only when the sender is carrying the broadcast, and a wildcard carries
  nothing, so a second field would be provably equal to the first.
- `Epoch` has no meaning for a wildcard, because there is no generation of
  content at a pattern. Say so explicitly rather than leaving a field a
  receiver might splice on.
- Remove the `Ended` flag from ANNOUNCE_REQUEST in the same lite-06 edit. It is
  unimplemented everywhere, and its one stated use, enumerating available
  recordings, is per-recording announce state, the fleet-wide growth
  wildcards exist to stop. A client that must distinguish recording
  generations reads them from the catalog the archive serves, not from
  announcements.
- Selection is two rules, in order. First, when several patterns match one
  path, only the tier selected by the matcher's shared structural specificity
  is consulted, with equal-specificity patterns forming one
  pool. A refusal from that tier never falls through to a less specific
  pattern, so a service refusing a path does not leak the request to a
  catch-all and one unserved path costs one round trip. Second, within the
  tier, several advertisers is the expected state, a worker pool rather than a
  contest: lowest accumulated cost orders the pool and a deterministic hash of
  the REQUESTED path against each advertiser distributes within it, so
  distinct paths spread rather than one advertiser taking the whole pattern. A
  wildcard is ranked by that same metric rather than by a rule of its own;
  what keeps a standby from outranking a running publisher is the size of its
  seed. State the floor normatively: a seed intended as a standby MUST exceed
  the maximum accumulated cost a bounded hop list can carry, and say what goes
  wrong when it does not. Selection applies to any request kind for a matching
  path, subscribe and FETCH alike, with the same tiering, hash, and refusal
  semantics; an archive answering FETCH for stored groups is the case that
  makes this explicit.
- Loop handling reuses the hop list unchanged: a receiver discards a wildcard
  whose reconstructed path contains its own hop id, and the per-subscriber
  origin exclusion applies the same way.
- A subscribe or FETCH an advertiser will not serve is a stream reset carrying
  a typed code, which the wire already supports (`Error::to_code`). Define a capacity
  refusal distinctly from the permanent ones already there (not found,
  unauthorized, unroutable): only it permits the receiver ONE re-resolution,
  within the same specificity tier and excluding the refuser. Say why the
  distinction is not a fallback list: an advertiser's capacity and a relay's
  view of it are always at least half a round trip apart, so a retraction and a
  request for the slot it just gave away necessarily cross. The exclusion is
  what makes the retry safe, not the retraction arriving first: say plainly
  that re-resolution may find no other advertiser and return unroutable, which
  is a correct outcome. Every other refusal is terminal, so scanning unserved
  paths still costs one round trip per path. An unrecognized code is permanent.
- Authorization: an advertised pattern MUST be contained by the sender's
  granted patterns, using the matcher's exact containment check. Apply the same
  rule to literal-headed and leading-star patterns and refuse widening rather
  than clamping it. This is how a cluster-scoped archive says "anything you
  cannot get live" and a transcoder advertises one suffix for every project.
- A wildcard is a CAPABILITY, not an inventory: it says what the sender could
  serve, never that a given path exists, which is what makes an over-claiming
  advertisement well-formed and refusal the way one path is denied.
- Wildcards are visible to subscribers rather than relay-internal state,
  rebased by the matcher's exact set-valued operation, and
  duplicate advertisements of one pattern combine into a single presented
  entry. A receiver MUST NOT present one as an available broadcast: it is a
  distinct kind of announcement, since a pattern names no content.
