The `/js` Bun workspace, published as `@moq/<package>`. Run recipes via `just js <recipe>`.

# Signals

`@moq/signals` is the reactive core; read `signals/src/index.ts` before touching reactive code.

- `Signal` equality is deep for plain data but identity for class instances; `set(v, true)` forces a notify.
- `Computed` is `undefined` until its first run and after `close()`. Close standalone ones; `effect.computed()` closes with its parent.
- An `Effect` rerun waits for every `effect.spawn` task from the previous run to settle, so register `effect.cleanup` unconditionally.
- Use the scoped helpers (`effect.interval`, `timer`, `event`, `subscribe`, ...) over raw timers and listeners, and nested `effect.run` over one giant effect.
- Never `Promise.race` a value that outlives the call, such as `closed`; use `race` or `effect.race`.

# Lifecycle

- Terminal state is one `closed: GetPromise<Error | null>`: `undefined` is open, `null` a clean close, an `Error` an abort. `if (closed)` means aborted; use `closed.peek() !== undefined` for closed.
- Every `close()` is idempotent. `Once` is a thenable, not a `Promise`.

# Components

- `watch`/`publish` components expose read-only `in` dependencies, `out = readonlys(...)` derived state, and writable `Signal` knobs (typed `T | Signal<T>` in props). Positional identity is a plain constructor arg. The `Effect` is private; `close()` is the handle.
- Custom elements use no framework; attributes are the public API. An invalid attribute value throws instead of falling back to a default. Attributes with a unit require one (`delay="100ms"`).
- Entrypoints re-export deps under namespaces (`Net`, `Signals`, `Hang`); re-export only public API. The `./element`, `./ui`, `./support` subpaths are side-effectful and stay out of the main entry.

# Conventions

- Only browser performance counts; accept a small regression that shows only on node or bun.
- Document every export and give each entrypoint a `@module` block (JSR builds docs from them). Hide deprecated exports with `@internal` or drop them; never annotate "use X".
- `just js check` includes the build, since declaration emit catches errors `--noEmit` misses, and rejects importing a `@moq/*` package the manifest doesn't declare.
- Test UI or playback changes with `just dev` in a real browser. `<moq-watch>` renders black in a background tab unless `visible="always"`.
