# [M] Advertise-only authorization

## Goal

A v1 worker credential can advertise an allowed wildcard without receiving
permission to publish any path matching it. Relays enforce advertise and
publish as independent capabilities before external processor credentials are
minted.

## Plan

Add an explicit advertise pattern union to the v1 claims, token SDKs, origin
scope, and relay authorization model. Wildcard authorization checks that
scope rather than borrowing the publish union. A concrete announcement or
publish request still requires publish permission, so an advertise-only worker
cannot bypass the demand exchange.

Preserve current customer credentials in the wire and authorization design:
existing claims retain their current publish-implies-advertise behavior, while
the new v1 claim separates the capabilities. Land the claims, SDK,
origin-scope, relay authorization, and tests without combining the release or
the moq.pro (downstream) pin rollout into this quest.

Cover containment, rebasing, leading-star and suffix patterns, missing versus
empty advertise scope, v0 compatibility, token revalidation, concrete announce,
publish, FETCH, and a wildcard demand that receives only an exact short-lived
publish grant.

## Required

- [Advertise](/quest/m2/wildcard/advertise.md) - supplies the wildcard message
  and authorization point this capability separates
- [Token SDKs](/quest/m2/path-patterns/token-sdk.md) - supplies the published
  v1 claim writers this extension changes
- [Relay auth](/quest/m2/path-patterns/relay-auth.md) - supplies the v1 origin
  scope and authorization readers this extension changes
