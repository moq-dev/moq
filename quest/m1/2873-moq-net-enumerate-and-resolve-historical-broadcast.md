# [M] moq-net: enumerate and resolve historical broadcast generations by path and epoch

## Goal

Implement and verify the behavior tracked in [#2873](https://github.com/moq-dev/moq/issues/2873)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Problem

Ordinary broadcast discovery has one current advertisement per path. With broadcast epochs, an origin can retain multiple generations under that path, but a new consumer still has no way to discover an older or ended generation.

This matters for recordings and offline VOD. A client should be able to enumerate historical generations without changing the ordinary live path into `<path>/<epoch>`, and without instantiating a broadcast front for every stored recording.

#### Contract

Add a separate history or VOD discovery surface that enumerates entries by:

- broadcast path
- non-zero epoch
- whether the generation is live or ended

Ordinary announcements remain unchanged in purpose and expose only the highest currently available epoch per path.

Resolution rules:

- `TRACK` and `FETCH` may explicitly resolve any retained generation.
- `SUBSCRIBE` accepts only a live generation.
- An explicit epoch never silently resolves to another generation.
- Epoch `0` means the current highest generation. Resolution happens before validating whether the requested operation is allowed.
- If the current generation is ended, `SUBSCRIBE(epoch=0)` fails rather than searching backward for an older live generation.
- A generation may transition from live to ended, but not back to live. Publishing new live content at the path requires a greater epoch.

Persistence, retention, and authorization remain application policy. The protocol and model only expose the retained generations that the application makes available.

#### Architecture constraints

This should build on the epoch model from #2610: path, then generation, then interchangeable routes. It should also compose with #2756 so a large offline catalog can advertise cheap entries without allocating a broadcast front per recording.

Do not overload ordinary route selection with history. Historical generation enumeration is a distinct query/API and will need its own wire design before implementation.

#### Acceptance cases

- A path with one live generation and multiple ended generations is announced normally as only its highest epoch.
- History enumeration returns each retained `(path, epoch, ended)` entry.
- A client can fetch an explicitly selected ended generation.
- A subscription to an ended generation is rejected.
- An explicit unknown epoch fails without falling back to current.
- An offline catalog can expose many historical generations without eagerly instantiating broadcasts.

Related: #2610, #2756, #2281, #2275, #2846.

## Required

- [Plan: historical broadcast generations](/quest/m1/plan-generations.md) - split into implementable quests first

## Closes

- [#2873](https://github.com/moq-dev/moq/issues/2873) - close this issue when the quest finishes
