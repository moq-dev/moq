# [XS] moqsrc reconnects correctly whatever order events arrive in

## Goal

A moq-gst test reconnects `moqsrc` while a rendition pad is blocked
downstream, delivering the reconnect's events (session close, new catalog,
restart, track end) in several orders, and the source always ends with one
live pad per rendition, no duplicate or leaked pads, and no deadlock.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7368](https://github.com/moq-dev/moq/pull/7368): m2, as
recommended. Start after [moq-dev/moq#7368](https://github.com/moq-dev/moq/pull/7368) (open) lands; it moves each rendition pad
into one actor (`rs/moq-gst/src/source/`).

Reuse [moq-dev/moq#7368](https://github.com/moq-dev/moq/pull/7368)'s restart/relist/retire/finish tests and their blocked-pad
setup. Enumerate the orders explicitly (a small table of permutations)
rather than relying on stress runs, and check pad count and EOS state after
each.

Public API: none. Wire: none.

## Related

- [moqsrc owns each rendition pad in one actor](/quest/m2/moqsrc-pad-actor.md) - [moq-dev/moq#7368](https://github.com/moq-dev/moq/pull/7368) lands the actor this tests
