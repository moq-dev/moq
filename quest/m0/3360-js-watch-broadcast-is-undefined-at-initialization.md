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
- Regression: a test that creates the element before the definition is
  registered, reads `broadcast` after `whenDefined` resolves, and asserts it
  is set.

The issue's side question, reacting to catalog changes from a framework, is
independent API work: [Catalog signal](/quest/m2/watch-catalog-signal.md).

## Closes

- [#3360](https://github.com/moq-dev/moq/issues/3360) - close this issue when the quest finishes

## Related

- [Catalog signal](/quest/m2/watch-catalog-signal.md) - the reactive catalog access the same report asked for
