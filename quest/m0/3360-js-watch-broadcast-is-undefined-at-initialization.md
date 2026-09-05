# [S] js/watch: MoqWatch.broadcast is undefined when a framework binds the element

## Goal

A framework that binds `<moq-watch>` and reads `moqWatch.broadcast` in its
first effect gets the `Broadcast` the type promises, or a documented reason
it cannot yet. On 0.5.2 the read returns `undefined`; it worked on 0.3.2.

## Plan

The obvious fix is already the implementation: `js/watch/src/element.ts`
constructs `this.broadcast = new Broadcast({..})` synchronously in the
constructor, so the field is never undefined on an upgraded element. What the
report shows is therefore a read before upgrade: Svelte's `bind:this` hands
the framework the raw `HTMLElement` as soon as it is in the DOM, and the
constructor only runs once `customElements.define` has registered the tag. If
the definition lands after the framework's first effect (a module-order
difference between 0.3.2 and 0.5.2 would explain the regression), every field
the class sets is absent until the upgrade.

- Reproduce with the issue's Svelte page and confirm the timing by logging
  `customElements.whenDefined("moq-watch")` against the effect.
- Fix whichever side owns it: if the entrypoint defers registration, register
  synchronously on import as 0.3.2 did; if the framework simply reads early,
  document `await customElements.whenDefined("moq-watch")` in `doc/lib/js`
  and make the element's own `connectedCallback` tolerate late upgrade.
- Regression on the ordering itself, not on the upgrade: the browser
  upgrades an existing node when the definition is registered, so a test
  that waits on `whenDefined` before reading passes either way. Assert
  instead that importing the entrypoint has defined `moq-watch` before the
  import resolves (`customElements.get("moq-watch")` is set synchronously
  after `import "@moq/watch/element"`), so a framework that mounts after the
  import can never observe a pre-upgrade node.

The issue's side question, reacting to catalog changes from a framework,
needs no new API: `broadcast.out.catalog` is already a read-only `Getter`,
which `demo/web` reacts to with `effect.get(watch.broadcast.out.catalog)`.
Document that in `doc/lib/js` as part of this quest, with a custom section
read through it.

## Closes

- [#3360](https://github.com/moq-dev/moq/issues/3360) - close this issue when the quest finishes
