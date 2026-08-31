# [S] js/publish: Fanout with a zero queue spins forever

## Goal

Implement and verify the behavior tracked in [#3171](https://github.com/moq-dev/moq/issues/3171)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Summary

On `dev` at `7494084a`, `Fanout.subscribe(effect, queue)` accepts a queue size without validating it. In `Fanout.#push`, the eviction loop is:

```ts
while (reader.queue.length >= reader.limit) {
    const dropped = reader.queue.shift();
    if (dropped !== undefined) this.#release?.(dropped);
}
```

With a limit of zero, an empty queue satisfies `0 >= 0`. `shift()` keeps returning `undefined`, so the loop never makes progress and the JavaScript event loop is permanently blocked. Negative limits behave the same way, and `NaN` silently disables the bound.

`Fanout` is reachable through the public audio and video capture outputs, while the API only describes the queue as a buffer count and does not require it to be positive.

Code: https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/js/publish/src/fanout.ts#L63-L64 and https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/js/publish/src/fanout.ts#L167-L174

#### Reproduction

```ts
const effect = new Effect();
const fanout = new Fanout(source);
fanout.subscribe(effect, 0);
sourceController.enqueue(1);
```

A bounded reproducer run under `timeout 2s` exited with status 124. The event loop never reached a timer scheduled for 10 ms later.

#### Expected

Either reject any non-finite, non-integer, or non-positive queue size at construction/subscription, or define and implement explicit zero-buffer behavior. Invalid input must not hard-lock the page.

Add a regression test for both the default constructor queue and the per-subscriber override.

## Closes

- [#3171](https://github.com/moq-dev/moq/issues/3171) - close this issue when the quest finishes
