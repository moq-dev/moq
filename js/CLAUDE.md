The `/js` TypeScript workspace. Extends the root `CLAUDE.md`.

# Packages

Bun workspaces listed in the repo-root `package.json`, published as `@moq/<component>` after the root list. Run recipes via `just js <recipe>`. Beyond that list: `@moq/signals` is the reactive core every package uses, `@moq/wasm` wraps `rs/moq-wasm`, and `@moq/boy` is the only Solid/`.tsx` package.

Entrypoints re-export deps under namespaces (`Net`, `Signals`, `Hang`). Only re-export what is part of the public API.

# Signals + Effect

The spine of the JS code; read `signals/src/index.ts` before touching reactive code.

- `Signal<T>`: `set`/`update`/`mutate` write, `peek` reads without subscribing. Writes coalesce per microtask and notify only on change. Equality is deep for plain data but identity for class instances. `set(v, true)` forces a notify.
- `Computed<T>`: derived, `undefined` until first run and after `close()`. Standalone ones must be closed; `effect.computed()` closes with its parent.
- `Effect`: reruns when a signal read via `effect.get(signal)` changes. Register teardown with `effect.cleanup(fn)`; it runs before the next run and on `close()`. A rerun waits for every `effect.spawn` task from the previous run to settle, so register teardown unconditionally.
- Use the scoped helpers (`effect.interval`, `timer`, `timeout`, `animate`, `event`, `subscribe`, `set`, `proxy`, `run`) instead of raw timers or listeners, so cleanup is automatic. Prefer nested `effect.run` over one giant effect.
- DEV warnings flag leaks: ~100 subscribers, an effect that tracked nothing, an `Effect` GC'd without `close()`.

# Producer / consumer

Networking objects split a plain `XxxState` of `Signal` fields from the public `Xxx` class. Terminal state is one `closed: GetPromise<Error | null>` backed by `Once`: `undefined` is open, `null` a clean close, an `Error` an abort. `if (closed)` means "aborted", not "closed"; use `closed.peek() !== undefined`. Every `close()` path is idempotent. `Once` is a thenable, not a `Promise`: `.then()` only.

# Components

Every reactive component in `watch`/`publish` follows one shape (see `publish/src/video/encoder.ts`):

- `readonly in`: wired dependencies, read-only to consumers.
- `readonly out = readonlys(...)`: derived state; never hand out a writable `Signal`.
- Knobs stay public writable `Signal`s (`encoder.config`), typed `T | Signal<T>` in props.
- Positional identity (a name, a kind) is a plain constructor arg, not a signal.
- `#signals = new Effect()` is private; `close()` is the only handle. The `<moq-watch>`/`<moq-publish>` elements are the exception and expose `signals`.

# Web components

Plain custom elements on `@moq/signals`, no framework. Attributes are the public API and mirror into signals in `attributeChangedCallback`. An invalid attribute value warns and falls back; never throw. Booleans reflect as bare attributes. Attributes with a unit take one (`delay="100ms"`), since `parseFloat("30s")` is 30. Create the `Effect` in `connectedCallback`, close it in `disconnectedCallback`. Styles import as `?inline` into a `ShadowRoot`. The `./element`, `./ui`, `./support` subpaths are side-effectful and not re-exported from the main entry.

# Conventions

- ESM only. Relative imports include the `.ts` extension in `net`, `signals`, `hang`; match the file you're editing elsewhere.
- Document every export and add a `@module` block to each entrypoint; JSR builds docs from them. Deprecated exports are marked `@internal` or dropped from the entrypoint, never annotated "use X".
- `bun` for everything. Biome formats and lints (repo-root `biome.jsonc`). Tests are `*.test.ts` under `bun test`; `tsconfig.build.json` only drops them from the emit.
- `just js check` type-checks, lints, and builds; the build is part of `check` because declaration emit catches errors `--noEmit` misses.
- For UI or playback changes, run `just dev` and exercise it in a real browser; WebTransport and WebCodecs only fail at runtime. `<moq-watch>` renders black in a background tab: set `visible="always"` or bring the window frontmost.
