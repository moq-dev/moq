# [XS] doc: observing the catalog from moq-watch

## Goal

A `<moq-watch>` user can find, in the published docs and the element's JSDoc,
how to observe the live catalog and how to read a custom catalog section from
it.

## Plan

Not a bug: `MoqWatch.broadcast` is assigned in the constructor. What moved in
0.4.0 (#2242, #2335) is the `Broadcast` class's signals into `in` and `out`
groups, so the catalog is `broadcast.out.catalog`, a read-only
`Signal<Catalog.Root | undefined>`, observed with
`moqWatch.signals.run((effect) => effect.get(moqWatch.broadcast.out.catalog))`.
The element's `catalog` accessor peeks it (and sets the manual-mode input);
`observedAttributes` carries `catalog-format` (`hang`, `msf`, `hangz`,
`manual`) and no `catalog`. A custom section rides the loose catalog schema
and is read off the same root object.

- `doc/`: the watch element page gains an "Observing the catalog" section with
  the `signals.run` snippet and a custom-section example, and the hang concept
  page's custom-tracks section links to it.
- JSDoc on `MoqWatch.broadcast`, `MoqWatch.catalog`, and
  `Broadcast.out.catalog` says how to observe; the `.d.ts` is what consumers
  read.
- No API change. A reactive accessor on the element is a separate decision if
  the docs prove insufficient.

## Closes

- [#3360](https://github.com/moq-dev/moq/issues/3360) - close this issue when the quest finishes
