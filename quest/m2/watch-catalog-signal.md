# [S] js/watch: the catalog is a signal on the element

## Goal

A framework can react to the catalog `<moq-watch>` is playing, including its
custom sections. Today `catalog` is a plain getter, `observedAttributes`
carries `catalog-format` but not `catalog`, and nothing on the element
notifies when the catalog changes, so reading a custom section (the documented
`doc/concept/layer/hang` custom-tracks case) means polling.

## Plan

Expose the catalog as a `Getter<Catalog.Root | undefined>` on the element in
the signals idiom `js/CLAUDE.md` describes, next to the existing getter, and
show in `doc/lib/js` how a framework subscribes to it and reads a custom
section. Keep the attribute surface as it is: the catalog is output, not
input, so it does not belong in `observedAttributes`.

Test: a catalog update on the broadcast is observed through the element's
signal, and a custom section round-trips through it.

## Related

- [#3360](/quest/m0/3360-js-watch-broadcast-is-undefined-at-initialization.md) - the report this was split from
