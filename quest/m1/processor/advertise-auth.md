# [M] Advertise-only authorization

## Goal

A v1 worker credential can advertise an allowed prefix claim without receiving
permission to publish any path beneath it. Relays enforce advertise and
publish as independent capabilities before external processor credentials are
minted.

## Plan

Add an explicit advertise pattern union to the v1 claims, token SDKs, origin
scope, and relay authorization model. A prefix claim is checked against that
scope rather than borrowing the publish union. A concrete announcement or
publish request still requires publish permission, so an advertise-only worker
cannot bypass the demand exchange.

Preserve current customer credentials in the wire and authorization design:
existing claims retain their current publish-implies-advertise behavior, while
the new v1 claim separates the capabilities. Land the claims, SDK,
origin-scope, relay authorization, and tests without combining the release or
the moq.pro (downstream) pin rollout into this quest.

Cover containment of a claimed prefix in the advertise scope, rebasing,
missing versus empty advertise scope, v0 compatibility, token revalidation,
concrete announce, publish, FETCH, and a claim's demand that receives only an
exact short-lived publish grant.
