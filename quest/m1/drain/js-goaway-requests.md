# [S] JS refuses new requests after GOAWAY

## Goal

Once a `@moq/net` session has received GOAWAY, it opens no new subscribe,
fetch, or announce-interest stream on that session, on lite and IETF, as
moq-net already refuses them with `Error::GoingAway`. Existing subscriptions
keep flowing until the handover ends, and a refused request is served by the
replacement session rather than failing the caller.

## Plan

The drain line's JS migration (https://github.com/moq-dev/moq/pull/4143)
keeps the old session serving after GOAWAY but left its subscribers unaware
of it, so during the handover an origin request can still open a stream on a
session the peer asked us to leave. The lite draft says the recipient must
not open new streams after GOAWAY, and a compliant draining relay rejects
them. Rust checks before every new open in both wires (`check_going_away` in
the lite subscriber, the `going_away` checks in the IETF one).

Guidance:

- The drain signal already reaches the connection wire view
  (`wireOf(session).goaway`). Hand it to both subscribers and check it at
  every open site, including the draft-14 to -16 adapter route.
- The refusal matters as much as the check: the origin should see it as this
  route declining, so the request moves to the replacement's route (the
  origin already keeps the outranked route until the new one answers). Match
  what the Rust origin does with `GoingAway` rather than inventing a policy.
- Test on lite and IETF with the existing mock transports: after GOAWAY a new
  request opens no stream on the old session and resolves on the new one,
  while an existing subscription keeps receiving groups.

No public API change is expected; the error is internal unless a caller can
observe it today.
