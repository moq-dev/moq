# [XS] JS fetchGroup stops when its grant is revoked

## Goal

A `@moq/net` subscriber-side `fetchGroup` holds a grant watch from its first
check to its end, as subscriptions do, so a fetch whose path leaves the
session's grant ends with `Unauthorized` instead of finishing.

## Plan

Found by #4560, which gave
every other JS request a watch armed before its first await. Follow the same
pattern, and add a regression test where the grant changes during setup and
mid-fetch.
