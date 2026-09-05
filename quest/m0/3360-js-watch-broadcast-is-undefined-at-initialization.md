# [S] js/watch: `MoqWatch.broadcast` exists from construction

## Goal

`moqWatch.broadcast` is usable the moment the element exists, as its
declared type `broadcast: Broadcast` promises. On 0.5.2 a framework effect
that reads `moqWatch.broadcast.catalog` right after binding the element hits
`broadcast is undefined`; it worked on 0.3.2.

## Plan

Either the field is created in the constructor, so it is never undefined,
or the type stops lying. Prefer the first: `connection` and `backend` are
constructed eagerly, and a `Broadcast` whose inputs are signals can exist
before any URL is set, so make `broadcast` the same. If some part of it
genuinely cannot exist before `connectedCallback`, split that part out and
keep the field itself eager.

The issue's side question is a real gap too: `catalog` is a getter, not a
signal, and `observedAttributes` carries `catalog-format` but not `catalog`,
so a framework cannot react to catalog changes. Expose the catalog as a
`Getter` on the element and document in `doc/lib/js` how to read a custom
catalog section from it.

Regression: construct the element, read `broadcast.catalog` before connecting
it, and subscribe to the catalog signal across a catalog update.

## Closes

- [#3360](https://github.com/moq-dev/moq/issues/3360) - close this issue when the quest finishes
