# [XS] A late subscriber to a closed JS track is released

## Goal

On `dev`, where `@moq/net`'s track retention can be unlimited, a subscriber
that joins a track after its producer closed is removed from the track cache
once it's done, instead of being held forever.

## Plan

Found while landing the main-into-dev sync (#4428). That PR stopped caching
a subscriber the producer closes when retention is unlimited, since nothing
ages it out, but a subscriber that arrives after the close takes a different
path in `js/net/src/track.ts` and is never cleaned up. Release it on the same
terms, and add a test that a late subscriber to a closed track leaves no
cache entry once it drops.

Public API: none. Wire: none. Targets `dev`, where unlimited retention lives.
