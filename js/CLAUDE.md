# js/CLAUDE.md

Scopes the `/js` TypeScript/JavaScript workspace. Universal rules (writing style / no em-dashes, Branch Targeting, Cross-Package Sync, AI Attribution, Public API Scrutiny, Refactor As You Go, comment/doc conventions) live in the root `CLAUDE.md` and are not repeated here.

## Workspace layout

Bun workspaces; members listed in the repo-root `package.json` (not in `js/`). Deps hoist to the repo root `node_modules`, not into `js/`. Run recipes via `just js <recipe>` (see `js/justfile`). Packages, grouped by role (each mirrors its `rs/` counterpart where one exists), roughly in dependency order:

**Foundation**

- `@moq/signals` (`signals/`): reactive core. `Signal`, `Computed`, `Effect`, plus framework adapters at subpaths `./solid`, `./react`, `./dom`. No deps on other workspace packages. Everything below uses it.

**Transport / protocol**

- `@moq/net` (`net/`): browser networking. Connect to a relay, then publish/consume broadcasts/tracks/groups/frames over WebTransport (WebSocket fallback). Negotiates `moq-lite` (`lite/`) or IETF `moq-transport` (`ietf/`). Mirror of `rs/moq-net`. Optional `zod` peer dep for `./zod` JSON-frame helpers.

**Container / catalog formats**

- `@moq/loc` (`loc/`): Low Overhead Container frame encoding. Thin layer on `@moq/net`.
- `@moq/json` (`json/`): snapshot/delta JSON over a track via RFC 7396 merge-patch. Exposes the base `Producer`/`Consumer` that `@moq/hang`'s catalog extends. DEFLATE via `@moq/flate`.
- `@moq/flate` (`flate/`): group-scoped DEFLATE primitive (only deps on `pako`). `Encoder`/`Decoder` turn a stream of payloads into self-delimited sync-flushed frames sharing one window; wire-interoperable with the Rust `moq-flate` crate. Used by `@moq/json`.
- `@moq/msf` (`msf/`): MOQT Streaming Format catalog types (zod schemas).

**Media**

- `@moq/hang` (`hang/`): WebCodecs media layer. Subpaths `./catalog`, `./container`, `./util`. Mirror of `rs/hang`. Catalog is a JSON track describing other tracks; container frames are timestamp + codec bitstream (CMAF under `container/cmaf`).
- `@moq/watch` (`watch/`): subscribe + decode + render, with optional UI. Subpaths `.`, `./element`, `./ui`, `./support`.
- `@moq/publish` (`publish/`): capture + encode + publish, with optional UI. Same subpath shape as watch.

**Apps / examples**

- `@moq/boy` (`moq-boy/`): MoQ Boy web viewer. The only package using `.tsx`/Solid.
- `@moq/clock` (`clock/`): private native example (publish/subscribe a clock).
- `@moq/token` (`token/`): JWT generation/validation (`jose`); also ships a `moq-token` bin. Mirror of `rs/moq-token`.

Top-level entrypoints re-export their deps under namespaces (`export * as Net from "@moq/net"`, `Signals`, `Hang`) so consumers get one import. `Lite`/`Moq` aliases are `@deprecated`, use `Net`.

## Signals + Effect (the reactivity model)

This is the spine of the JS code; read `signals/src/index.ts` before touching reactive code.

- `Signal<T>`: mutable observable. `set`/`update`/`mutate` write, `peek` reads without subscribing. Writes are coalesced per microtask; subscribers fire only when the value actually changed. Equality is deep for plain objects/arrays but identity (`===`) for class instances (two `Broadcast` instances are never equal). Force a notify with `set(v, true)`; suppress with `set(v, false)`. `Signal.from(x)` wraps non-signals; cross-package-version identity uses a `Symbol.for` brand, not `instanceof`.
- `Computed<T>`: read-only derived signal. Its `fn` reads deps with `effect.get(...)` just like an effect. Value is `undefined` until the first run completes and after `close()`; always handle the `undefined` case. A standalone `Computed` must be `close()`d; one made via `effect.computed()` is closed with its parent.
- `Effect`: runs `fn(effect)`, reruns whenever a tracked signal changes. Track deps inside `fn` with `effect.get(signal)` (returns current value and subscribes). `effect.getAll([...])` reads several and returns `undefined` if any is falsy.

Lifecycle and cleanup (the rules that actually bite):

- Register teardown with `effect.cleanup(fn)`. Everything registered during a run is torn down before the next run and on `close()`. `close()` is permanent; reruns are not.
- Use the Effect-scoped helpers instead of raw timers/listeners so cleanup is automatic: `effect.interval`, `effect.timer`, `effect.timeout`, `effect.animate`, `effect.event(target, type, listener)` (merges an `AbortSignal`), `effect.subscribe(sig, fn)` (runs now + on change), `effect.set(sig, value, cleanup)`, `effect.proxy(dst, src)`. Do NOT reach for raw `setInterval`/`setTimeout`/`requestAnimationFrame`/`addEventListener` inside an effect.
- Nesting: `effect.run(fn)` / `effect.computed(fn)` create child scopes closed with the parent. Prefer nested effects over one giant effect so unrelated deps do not re-trigger each other.
- Async: `effect.spawn(() => Promise<void>)` runs a task and blocks the next rerun until it settles (warns after 5s). `effect.cancel` (promise) and `effect.abort` (`AbortSignal`) fire when the current run is torn down; `effect.closed` resolves on `close()`.
- DEV warnings catch leaks: a signal passing ~100 subscribers throws ("may be leaking"); an effect that subscribed to nothing warns ("will never rerun"); a `FinalizationRegistry` warns if an Effect is GC'd without `close()`. If you see these, you forgot a `close()` or tracked the wrong thing.

## Producer / consumer and pub/sub shapes

Networking objects split state from behavior: a plain `XxxState` class holds `Signal` fields, and the public `Xxx` class wraps it (see `net/src/broadcast.ts`, `track.ts`, `group.ts`). The publisher side answers `requested()` (await the next subscribed track) and writes; the consumer side `subscribe(name, priority)`s and reads. `closed` is exposed both as a `Signal` on state and as a `Promise` on the object. `@moq/json` and `@moq/hang/catalog` follow the same `Producer`/`Consumer` pair, with hang's catalog `Producer`/`Consumer` extending json's generics.

## Web Components UI (watch/ui, publish/ui)

Plain custom elements built directly on `@moq/signals`, no framework (except moq-boy, which uses Solid). The pattern, from `watch/src/element.ts` and `watch/src/ui/element.ts`:

- `class Foo extends HTMLElement` with `static observedAttributes`. Attributes are the public API; mirror each into a `Signal` in `attributeChangedCallback`.
- Create the `Effect` in `connectedCallback`, call `effect.close()` in `disconnectedCallback`. A module-level `FinalizationRegistry` closes the Effect if the element is GC'd without disconnect (there is no real destructor for custom elements).
- Build DOM with `@moq/signals/dom` (`create`, reactive helpers) and drive visibility/content from `effect.get(...)` inside `effect.run(...)`. UI components are functions `(parent: Effect, host) => HTMLElement` that register their own reactivity on `parent` (see `watch/src/ui/components/*`).
- Styles are imported as `?inline` CSS strings into a `ShadowRoot`. The `./element` / `./ui` / `./support` subpaths are side-effectful (they call `customElements.define`); the package marks them in `sideEffects` and they are NOT re-exported from the main entry (import from the subpath). These web-component packages set `"jsr": false` because JSR forbids the `HTMLElementTagNameMap` augmentation custom elements need.

## Conventions

- ESM only (`"type": "module"`). Relative imports include the `.ts`/`.tsx` extension in the lower-level packages (`net`, `signals`, `hang`); `rewriteRelativeImportExtensions` in `tsconfig.json` rewrites them to `.js` on build. Some higher-level packages (watch/publish) still omit extensions, so match the file you are editing.
- Document every exported symbol and add a top-of-file `@module` doc block to each entrypoint (root convention; the published JSR/`.d.ts` docs render these). Use `@public` on the load-bearing classes.
- **Deprecation mechanics** (root Deprecation explains the why): mark a deprecated export `@internal` or drop it from the entrypoint re-exports so it falls off the published JSR/`.d.ts` docs. No "deprecated, use X" note in its doc comment.
- Build is per-package: `tsc -b` (or `vite build` for the bundled UI/web-component packages) then `bun ../common/package.ts`, which rewrites `package.json` exports from `./src/*.ts` to built `./*.js`/`.d.ts` and runs `publint`. Release via `bun ../common/release.ts`.

## Tooling and testing

- Use `bun` for everything (install, scripts, test runner). Never npm/yarn/pnpm.
- Biome handles formatting and linting; config is the repo-root `biome.jsonc` (tabs, width 4, line length 120). `just fix` runs `bun biome check --write`.
- Tests are `*.test.ts` run by `bun test`. Add tests where easy (signals, varint, path, ring buffers, sync all have them).
- `just js check` type-checks + biome-checks every package; `just js test` runs all unit tests; `just js build` builds all. From repo root these are `just check` / `just fix` / `just build`.
- For UI / web changes (`watch`, `publish`, `demo/web`, anything touching playback or the `<moq-watch>`/`<moq-publish>` components), don't stop at unit tests: run `just dev` and exercise the change in a real browser via the Claude-in-Chrome plugin (if installed), since WebTransport + WebCodecs playback only surfaces at runtime. The Chrome plugin drives a real visible tab, so it avoids the headless-preview gotcha where `<moq-watch>` gates video download on a `hidden` tab.
