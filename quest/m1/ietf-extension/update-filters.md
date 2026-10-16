# [XS] Refuse filters on REQUEST_UPDATE

## Goal

A moq-transport REQUEST_UPDATE that carries a non-empty Range Filter (a
Subgroup Filter included) ends that subscription with an error instead of
being applied unfiltered, matching how SUBSCRIBE and FETCH already refuse
filters with INVALID_FILTER.

## Plan

- `rs/moq-net/src/ietf/subscribe.rs` decodes the filters on REQUEST_UPDATE
  (`_subgroup_filter` and friends, around line 516) and drops them.
- End the subscription the way other supported-but-unimplemented update
  preferences end it, with a regression test that fails today.
