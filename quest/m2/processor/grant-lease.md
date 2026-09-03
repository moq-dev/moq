# [L] Expiring media grants

## Goal

An exact native-worker v1 grant deadline applies to media handles already
opened with that grant. A worker cannot keep reading or publishing after demand
ends merely by opening the source and output before its token expires, and its
deadline never attaches to a pooled HTTP or HLS consumer.

## Plan

Give processor grants a native-worker audience that HTTP and HLS gateways
reject before opening or reusing a consumer. On a native worker session, an
accepted unpooled `Consumer` or `Producer` retains its grant deadline and is
cancelled when that deadline passes. A refresh with a fresh relay demand
assertion may atomically extend the same exact source and output handles; a
missing, late, broader, or mismatched refresh closes them. Reconnecting with an
expired grant is denied as it is today.

Keep deadline enforcement in the relay authorization owner rather than a
cooperative worker timer. Cover an idle open handle, active source reads,
active publication, refresh before expiry, refresh after demand ends, relay
clock skew within the token policy, disconnect races, HTTP and HLS rejection of
the worker audience, and unrelated traffic continuing through an existing
pooled HLS consumer after a worker grant expires.

Land the implementation and tests here. The release and the moq.pro
(downstream) pin rollout stay out of this quest.

## Required

- [Token SDKs](/quest/m2/path-patterns/token-sdk.md) - supplies exact v1 grant
  minting and the audience shape this lease extends
- [Relay auth](/quest/m2/path-patterns/relay-auth.md) - supplies the v1 native
  authorization owner that enforces handle deadlines
