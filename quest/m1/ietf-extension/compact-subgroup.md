# [XS] Refuse a compact-form subgroup other than 0

## Goal

A moq-transport subgroup stream whose header names its subgroup by its first
object ID (the compact form) is refused when that ID is nonzero, exactly as
an explicit nonzero Subgroup ID already is. Nothing is relabelled as
subgroup 0.

## Plan

- Today `recv_group` in `rs/moq-net/src/ietf/subscriber.rs` refuses an
  explicit nonzero Subgroup ID (`Error::Unsupported`, stream stopped, session
  kept; test `a_non_zero_subgroup_is_stopped_without_closing_the_session`),
  but never reads the compact flag (`has_subgroup_object`). A compact stream
  for subgroup N > 0 is taken as subgroup 0 starting at object N, and if it
  opens the group it is forwarded downstream as subgroup 0. moq-test mode 1
  (one subgroup per object, compact form) hits this.
- Refuse it the same way, with a regression test that fails today. A compact
  stream with no objects names no subgroup; treat it as subgroup 0.
- Check whether NOT_SUPPORTED is a better stop code than INTERNAL_ERROR in
  every supported draft, and use it if so.
- State in `doc/concept/standard.md` that only subgroup 0 is accepted.
