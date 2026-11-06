# [XS] The over-limit PUBLISH refusal invites a retry

## Goal

The moq-transport refusal of a PUBLISH over the listed-track limit
(`EXCESSIVE_LOAD`, "too many listed tracks", `rs/moq-net/src/ietf/subscriber/tracks.rs`)
carries a non-zero retry interval, as draft-18 section 10.6.2 says it
SHOULD, instead of 0, which asks the peer never to retry.

## Plan

Decided 2026-11-06 (maintainer, /quest-complete, a follow-up of
[moq-dev/moq#7993](https://github.com/moq-dev/moq/pull/7993), which kept it
out of scope):

- ✅ **Send the may-pass interval.** Over-limit is a refusal that may pass
  once the peer unlists a track, so it sends the same interval #7993 sends
  every refusal that may pass (`RETRY_INITIAL`, 1 s), from the same rule
  rather than a new constant. Not chosen: keeping 0 and documenting the
  deviation; a separate, longer constant for load refusals.
- ✅ m1: additive, no semver break.

Starts once #7993 lands. A test asserts the refusal's decoded retry interval
on each moq-transport version that has one.

Public API: none. Wire: the field's value only.
