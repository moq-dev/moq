# [S] An application claim's request cannot be accepted

## Goal

Answering an application claim's request with `accept` fails to compile in
Rust and to typecheck in TypeScript, instead of panicking (Rust) or throwing
(JS) at runtime. `origin::Producer::dynamic(prefix)` hands out requests that
only `prepare` and `reject`; the session-forwarded claim from
`dynamic_route` / `dynamicRoute` keeps `accept`. Behavior is otherwise
unchanged: the escape hatches stay hidden until
[Public routes](/quest/m1/public-model/routes.md) gives them a
public shape.

## Plan

Decided 2026-10-21 (maintainer), while moving a downstream application onto
[#5678](https://github.com/moq-dev/moq/pull/5678): its handler's
`request.accept(..)` on a `dynamic()` claim still compiled against the new
API and would have panicked on the first request in production. #5678's
decision 1 kept `accept` `#[doc(hidden)] pub` as the interim, guarded only by
the runtime check. Rejected: folding this into answer.md, which leaves the
trap until that [L] lands; opening the code PR without a quest.

Kept from #5678: prepare and publish, one epoch per answer, and the accepted
cold-start reset until
[announce takeover](/quest/m1/announce-takeover.md) lands. The downstream
check confirmed the reset is recoverable: a reader that follows the
announcement reaches the published answer, while a one-shot reader (an HTTP
fetch) cannot and gets `Unroutable`.

Guidance: split the request type by the handle that yields it, for example
`dynamic_route` returning its own handle whose request carries `accept`,
rather than one `Request` with an optional publisher. Delete the runtime
assert and its test once the type makes the call unreachable; a
compile-fail doctest (Rust) and a `@ts-expect-error` line (JS) take their
place. Bindings (moq-ffi, moq-c) already expose no `accept` and need no
change.

## Related

- [Public routes](/quest/m1/public-model/routes.md) - later removes the hidden peer-claim API this quest narrows
