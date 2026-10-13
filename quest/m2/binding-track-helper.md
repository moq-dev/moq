# [M] Bindings resolve a rendition's track name

## Goal

A binding app subscribes to a catalog rendition without spelling out
`track ?? key` itself. Rust (`hang::catalog::Reference::track`) and JS
(`Catalog.track`) already resolve a rendition's track name from its ID and
optional `track`. The bindings (moq-ffi, moq-c, Python, Go, Swift, Kotlin,
Dart, C++) instead document the rule and leave every caller to inline it,
so a caller that passes the catalog key subscribes to the wrong track
whenever `track` is set.

## Plan

Open question, recommendation first:

1. Make the mistake unrepresentable: the container consumer and group fetch
   take the rendition (its ID and config) instead of a raw track name, and
   resolve it in moq-ffi. Every wrapper inherits it, and the docs stop
   describing the rule. Recommended.
2. Add a `track` helper to the FFI records (`MoqVideo` / `MoqAudio`) and
   wrap it in each language. Callers still pass a raw name, but have one
   obvious way to get it.

Either way, update the samples in `doc/lib/{py,swift,kt,go,dart,c,cpp}`,
`py/moq-rs/README.md`, and the `resolve` docs, which inline `track` or the
key today. Follow the cross-package sync table for `rs/moq-ffi`.

Starts after #5367 merges; it adds `track` to the FFI records.

Public API: per the option chosen, in moq-ffi and every wrapper. Wire: none.

## Related

- [Audio decode delay in every binding](/quest/m1/binding-surface.md) - another change touching every wrapper once
