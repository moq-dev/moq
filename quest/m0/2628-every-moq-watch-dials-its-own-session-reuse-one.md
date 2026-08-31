# [M] Every moq-watch dials its own session: reuse one WebTransport connection per relay URL and…

## Goal

Implement and verify the behavior tracked in [#2628](https://github.com/moq-dev/moq/issues/2628)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Summary

Every `<moq-watch>` constructs its own `Connection.Reload` in its constructor, so a page showing N tiles from one relay dials N WebTransport sessions to the same URL. `connect()` even passes `allowPooling: false` to `new WebTransport(...)`, so the browser's own HTTP/3 pooling is opted out too: one session per element is explicit, not incidental. And because each session's lifetime is tied to its element's connected state (#2627), a detach that outlives a microtask closes the session.

> we could certainly make some easy changes, like reusing the WebTransport connection instead of dialing a separate one per component, and keeping it alive while not in the DOM.

#### Proposal

A shared connection pool keyed by relay URL:

- The element (or `Connection.Reload` itself) looks up an existing session for its `url` and takes a reference instead of dialing. Last reference out closes it.
- A short grace period before an unreferenced session actually closes, a second or two, or until `pagehide`. That makes DOM moves free as a side effect, since the moved element re-acquires the same session before the grace period ends, and it covers swapping one tile for another on the same relay without a re-handshake.
- Broadcast subscriptions stay per element. Only the QUIC session is shared.

#### Why multiview hits this

A camera wall is the worst case for per-element sessions: 4 to 16 elements, one relay, and users constantly rearranging tiles. Sharing the session cuts the handshakes and the relay's per-connection cost, and it makes the element indifferent to reparenting without any lifecycle cleverness.

We currently avoid all of this by never moving elements (fixed DOM order, layout as absolute positioning). I know the JS API is the recommended path for this level of control, but connection reuse seems to belong in the library either way: there is no supported way to do it from the outside. The element's `connection` field is public and writable, but `Broadcast` and `Sync` capture `connection.established` by reference at construction, so reassigning it does not rewire the pipeline, and there is no constructor argument or attribute for it either.

## Closes

- [#2628](https://github.com/moq-dev/moq/issues/2628) - close this issue when the quest finishes

## Related

- [#2627: The <moq-watch> re-insertion grace is one microtask: a detach spanning…](/quest/m0/2627-the-moq-watch-re-insertion-grace-is-one-microtask-a.md) - related open work
