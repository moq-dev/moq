# [XS] Drop dead worklet config

## Goal

Delete build config that no source uses:

- `js/moq-boy/vite.config.ts` registers `worklet()`, but moq-boy imports no
  `?worklet`. It externalizes `@moq/watch`, whose own build handles the
  render worklet.
- `js/hang/src/worklet.d.ts` and the `?worker&url` block in
  `js/hang/src/vite-env.d.ts` declare modules that nothing imports.

Confirm each one is unused (grep, and `just check`) before deleting it.
