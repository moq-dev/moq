# [M] js/flate: "codec rejects frames that inflate past the default cap" times out under parallel test load

## Goal

Implement and verify the behavior tracked in [#2838](https://github.com/moq-dev/moq/issues/2838)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

`just test` fails intermittently on `@moq/flate`, and it is a timeout rather than a logic failure:

```
@moq/flate test: (fail) codec rejects frames that inflate past the default cap [5273.79ms]
@moq/flate test:  8 pass
@moq/flate test:  1 fail
```

5273ms against bun's 5000ms default per-test timeout. It squeaks under the limit on an idle machine and blows it when the rest of the suite is competing for CPU.

#### Why it's slow

`js/flate/src/index.test.ts` builds a 64 MiB decompression bomb:

```ts
const slice = encoder.frame(enc.encode("a".repeat(64 * 1024 * 1024 + 1)));
expect(() => decoder.frame(slice)).toThrow(/exceeded/);
```

That is a 64 MiB string allocation, a UTF-8 encode, a deflate, and a bounded inflate. Inherently CPU-bound, so it scales with machine contention rather than with anything the test is checking.

#### Evidence it's load, not logic

- In isolation the whole file runs in **1.99s, 9/9 pass**, repeatedly.
- Under a full parallel `just test` (and again under `just js test`) it reproduces at ~5.3s.
- The assertion itself is about the *cap* being enforced, which does not require the payload to be anywhere near 64 MiB.

Observed while running `just test` for #2814, a PR touching only `rs/moq-wasm` and READMEs, with no path to `@moq/flate`. That PR's CI is green, so this appears to be a local/loaded-runner condition rather than something CI hits today, but it costs a full re-run every time it bites.

#### Options

- Drop the payload to just over the default cap instead of 64 MiB. The decoder bounds output *as it is produced*, so a payload slightly past the limit exercises the same path far more cheaply.
- Or give this one test an explicit generous timeout, so it is deliberate rather than accidentally sitting 5% under the default.

The first is better: it makes the test fast *and* keeps it honest, rather than making a slow test tolerated.

## Closes

- [#2838](https://github.com/moq-dev/moq/issues/2838) - close this issue when the quest finishes
