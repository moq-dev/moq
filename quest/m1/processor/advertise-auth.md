# [M] Advertise-only authorization

## Goal

A v1 worker credential can advertise an allowed prefix without receiving
permission to publish any path under it. Relays enforce advertise and publish
as independent capabilities before external processor credentials are
minted.

## Plan

Add an explicit advertise prefix scope to the v1 claims, token SDKs, origin
scope, and relay authorization model. [Wildcard](/quest/m0/wildcard/README.md)
checks advertisements against `moq_auth::Claims.publish` today; this quest
gives them their own scope instead of borrowing the publish one. A concrete
announcement or publish request still requires publish permission, so an
advertise-only worker cannot bypass the demand exchange.

Decided: the advertise scope is prefix-only. Advertising is prefix-only on
every wire (Wildcard's decision) until [announcement shapes](/quest/m3/announce-shapes.md)
adds exact and suffix shapes to moq-lite, so leading-star and suffix advertise
patterns have nothing to authorize yet; that quest extends this scope. Token claim patterns
keep their suffix support for publish and subscribe.

Decided: an advertised prefix must overlap the advertise scope, not sit
inside it, and a wider claim only routes the requests the scope covers. This
matches how the relay authorizes advertisements today.

Preserve current customer credentials in the wire and authorization design:
existing claims retain their current publish-implies-advertise behavior, while
the new v1 claim separates the capabilities. Land the claims, SDK,
origin-scope, relay authorization, and tests without combining the release or
the moq.pro (downstream) pin rollout into this quest.

Cover overlap and per-request filtering, rebasing, missing versus empty
advertise scope, v0 compatibility, token revalidation, concrete announce,
publish, FETCH, and a prefix demand that receives only an exact short-lived
publish grant.

## Required

- [Wildcard](/quest/m0/wildcard/README.md) - the prefix advertisements this scopes
- [Auth](/quest/m1/auth/README.md) - the v1 claims and relay authorization this extends
