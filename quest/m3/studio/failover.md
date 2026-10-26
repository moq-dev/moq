# [M] Relay failover

## Goal

`moq-studio` publishes the same broadcast epoch to two relays at once, so
when one relay or the path to it fails, viewers resume from the other
mid-group and the stream does not drop.

## Plan

Publisher epochs already support this: the same epoch on several relays
resumes mid-group, and a restart is a newer epoch (`doc/concept/moq-lite.md`,
Publisher epochs and Multiple CDNs). The studio dials one relay today. Encode
once and publish to both. The second relay is configured, never inferred.
Both links must negotiate a version with publisher epochs; a relay that does
not is refused at setup with the reason, rather than failing over as a
restart. Show
each relay's state in the health panel.

## Related

- [Stream health](/quest/m3/studio/health.md) - where each relay's state shows
