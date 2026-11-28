The `/js` Bun workspace (`@moq/<package>`). Run recipes via `just js <recipe>`. Generic TS rules live in `quest context ts`.

# Signals (`@moq/signals`)

Read `signals/src/index.ts` before touching reactive code.

- `Signal` equality is deep for plain data, identity for class instances (except clock values, by value). `set`/`update` notify on change; `mutate` and `set(v, true)` always notify.
- `Computed` is `undefined` until first run and after `close()`. Close standalone ones; `effect.computed()` closes with its parent.
- An `Effect` rerun waits for every `effect.spawn` task from the previous run to settle, so register `effect.cleanup` unconditionally. Use scoped helpers (`effect.interval`, `timer`, `event`, `subscribe`, `effect.race`) and nested `effect.run` over raw timers/listeners or giant effects.
- Terminal state is one `closed: GetPromise<Error | null>` (`undefined` open, `null` clean close, `Error` abort; check `closed.peek() !== undefined` for closed). `Once` is a thenable, not a `Promise`.

# Components & Checks

- `watch`/`publish` components expose read-only `in`, `out = readonlys(...)`, and writable `Signal` knobs (`T | Signal<T>` in props), with private `Effect` and `close()` handle (`<moq-watch>`/`<moq-publish>` elements expose `signals`).
- Custom elements use no framework; invalid attribute values throw instead of defaulting, and unit attributes require units (`delay="100ms"`).
- Entrypoints give a `@module` block and re-export public deps under namespaces (`Net`, `Signals`, `Hang`); keep side-effectful `./element`, `./ui`, `./support` out of main entries. Hide deprecated exports with `@internal` or drop them.
- Only browser performance counts (small node/bun-only regressions are acceptable). `<moq-watch>` renders black in background tabs unless `visible="always"`.
- `just js dead-code` (fallow, in `just check`) rejects unused files, exports, types, members, or deps: delete what it finds (annotate intentional uncalled public members with `// fallow-ignore-next-line unused-class-member`, or add script/Rust entrypoints to `.fallowrc.jsonc`).
