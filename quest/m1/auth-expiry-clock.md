# [S] Grant expiry is one fixed deadline everywhere

## Goal

A grant's expiry is a deadline fixed when the grant arrives, in both
`moq-auth`'s `Client::drive` and the relay, and both honour `moq-auth`'s 5 s
clock-skew allowance. Today `Client::drive` recomputes the time left from the
wall clock on every re-check, so the countdown restarts, and the relay ignores
the skew allowance, so a grant just past expiry stays live in the client but
ends immediately in the relay.

## Plan

- Build on the lease clock `lease::Producer` owns (#3943): it sets
  its deadline once per grant and measures it on the tokio clock, so the
  client's outage tests run on a paused clock like the relay's (#3969 fixed
  the relay side).
- Apply the skew allowance in one place both sides share.
- Test: re-polling keeps the deadline; a grant within the skew window is live
  on both sides; the client outage tests run on a paused clock.

## Required

- The relay's fixed expiry deadline (#3969) has merged
