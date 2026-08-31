# [S] js/net: a terminal Reload failure can reconnect after closed rejects

## Goal

Implement and verify the behavior tracked in [#3172](https://github.com/moq-dev/moq/issues/3172)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Summary

On `dev` at `7494084a`, `Reload.#retry` treats an authorization rejection or an expired retry window as terminal by rejecting `Reload.closed` and releasing the subscribe origin's expectation. It does not stop `#signals` or mark the loop terminal.

The connection effect remains subscribed to `url`, `enabled`, `#suspended`, and `#tick`. A later URL update, enabled toggle, or browser resume therefore runs `#connect` again after `closed` has permanently rejected.

Code: https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/js/net/src/connection/reload.ts#L309-L325

#### Reproduction

A focused test used a WebTransport stub that rejects every dial, a 1 ms retry timeout, and then changed the URL after awaiting the terminal rejection:

```ts
const reload = new Reload({
    url: new URL("https://example.com/one"),
    websocket: { enabled: false },
    delay: { initial: 1, max: 1, timeout: 1 },
});

await expect(reload.closed).rejects.toThrow();
const terminalDials = dials;

reload.url.set(new URL("https://example.com/two"));
await settle();

expect(dials).toBe(terminalDials);
```

Observed: `terminalDials` was 2, then the URL change caused a third dial. The assertion received 3.

#### Impact

The documented terminal lifecycle is false: work resumes after callers have observed that the loop is closed. With a subscribe origin, `#expected` has already been released, so the origin can report requests as unroutable while the resurrected session is connecting or active. Shared connections can also be retriggered by page lifecycle signals after an authorization rejection.

#### Expected

A terminal path should permanently stop the reactive connection effect before settling `closed`, while preserving idempotent cleanup and the terminal error. Add regression coverage for URL updates, enabled toggles, and browser resume after both authorization rejection and retry timeout.

## Closes

- [#3172](https://github.com/moq-dev/moq/issues/3172) - close this issue when the quest finishes
