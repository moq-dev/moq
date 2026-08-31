# [M] The moq-watch re-insertion grace is one microtask: a detach spanning any yield closes and…

## Goal

Implement and verify the behavior tracked in [#2627](https://github.com/moq-dev/moq/issues/2627)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Summary

`<moq-watch>` already tries to survive a DOM move. The source comment on `disconnectedCallback` says so: "Stop everything but don't actually cleanup just in case we get added back to the DOM." But the actual grace window is a single microtask checkpoint. Detach a playing element and re-insert it after any yield (an `await`, a timeout, a rAF, a framework that batches DOM ops) and the WebTransport session closes and redials: the tile goes black and rejoins.

A fully synchronous move (`otherParent.appendChild(el)` on a connected element) survives, but only by accident: `@moq/signals` coalesces notifications onto a microtask and drops a net-zero change, so the enabled signal's true to false to true flip inside one job is never observed. That behavior is pinned by a test in the signals package ("no notification when the net value is unchanged"), but nothing in the element documents it as a contract. So whether a move costs you the session currently depends on whether anything yields between the remove and the insert, which is a fragile line to build a layout on.

#### Environment

- `@moq/watch` 0.4.0, `@moq/net` 0.2.2, `@moq/signals` 0.2.0 (npm), Chrome stable
- The relay is not involved; the close originates client-side

#### Steps to reproduce

```html
<script type="module">import "@moq/watch/element";</script>

<div id="a">
  <moq-watch url="https://RELAY/anon" name="demo/angle1"><canvas></canvas></moq-watch>
</div>
<div id="b"></div>
<button id="move">move</button>
<script type="module">
  const el = document.querySelector("moq-watch");
  document.getElementById("move").onclick = async () => {
    el.remove();
    await Promise.resolve(); // any yield here: an await, setTimeout, rAF
    document.getElementById("b").appendChild(el);
  };
</script>
```

Press the button while the broadcast is playing. The session closes, a new one dials and rejoins. Replace the handler body with a bare `document.getElementById("b").appendChild(el)` and the session survives, per the coalescing above.

#### Mechanism

In `element.js`, the lifecycle callbacks only flip an enabled signal, and the connection is constructed with it:

```js
this.connection = new Moq.Connection.Reload({ enabled: this.#enabled });
// ...
connectedCallback()    { this.#enabled.set(true); /* re-applies display/position */ }
disconnectedCallback() { this.#enabled.set(false); }
```

The same signal also gates `Broadcast`. In `connection/reload.js`, `#connect` is one long-lived effect that tracks `enabled`; a rerun first runs the previous run's cleanups, one of which is `connection.close()`, and then, if enabled, spawns a fresh `connect()`.

In `@moq/signals`, `Signal.set` does not notify synchronously: it queues one flush on a microtask, and the flush returns early when the value is back to what it was at the start of the window. Hence: same-job flip means no rerun and no close; any yield between false and true means the flush observes false, the effect reruns, and the cleanup closes the session before the re-insert dials a new one.

#### Possible fixes

1. Make the grace deliberate instead of a coalescing accident: defer the disable to a task boundary, or a small linger, and cancel it if the element is connected again by then. A `queueMicrotask` guard is not enough, since the signal flush is already a microtask; something like a zero timeout checking `this.isConnected` would cover every same-frame reparent regardless of how the caller schedules it.

2. Define `connectedMoveCallback()`. Since Chrome 133, `Element.moveBefore()` calls it instead of the disconnected/connected pair; without it the spec fallback still fires both. Today that pair happens to coalesce and survive, but a defined callback would make the guarantee explicit rather than emergent.

3. Failing either, document the contract. "A synchronous move survives, anything async does not" is currently discoverable only by reading the signals package's flush logic.

4. The bigger version is sharing the connection and letting it outlive a detach entirely, filed separately: #2628.

#### Workaround

We render all tiles once in a fixed DOM order and make every rearrange, resize, swap and maximize a style change (absolute positioning by percentage), so a live element never detaches at all. It works, but the whole layout system ends up built around this one behavior.

I know the JS API is the intended path for apps that want this much control, and that may well be the answer here. Filing it anyway because the web component is the first thing people reach for, and the survival of a move depending on microtask timing is surprising.

## Closes

- [#2627](https://github.com/moq-dev/moq/issues/2627) - close this issue when the quest finishes

## Related

- [#2628: Every <moq-watch> dials its own session: reuse one WebTransport…](/quest/m0/2628-every-moq-watch-dials-its-own-session-reuse-one.md) - related open work
