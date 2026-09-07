# [S] js/net: an announcement event carries its typed status

## Goal

`Announced.Event` in js/net says whether a route is `active`, `restart`, or
`ended`, matching the three states Rust models and the wire carries. Today
the event is `{ prefix, active: boolean }`, so a restart is indistinguishable
from a steady active route and nothing downstream can react to it.

## Plan

Branch from dev: the event shape is public and this is breaking.

- `js/net/src/announced.ts` `Event` gains a `status: "active" | "restart" |
  "ended"` discriminant; `active` goes rather than lingering as a second
  source of truth. The lite and IETF announce decoders map the wire states to
  it, and the producer side (`origin.ts`, the announce handle from
  [JS announce](/quest/m1/js-announce.md)) emits `restart` where Rust does.
- Consumers: `js/watch`, `js/publish`, `js/hang`, `demo/web`, and the
  `doc/lib/js` pages that read `active`.
- Tests in `announced.test.ts` for each state and for a restart arriving on
  a live route.

## Closes

- [#2216](https://github.com/moq-dev/moq/issues/2216) - close this issue when the quest finishes

## Related

- [JS announce](/quest/m1/js-announce.md) - the producer-side handle that emits these states
- [#2318](/quest/m1/2318-js-net-remaining-capability-gaps-vs-rs-moq-net-setup-role.md) - the other js/net gaps
