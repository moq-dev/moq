# [XS] @moq/video loads under Node, and every published tsc package is load-checked

## Goal

The published `@moq/video` loads under Node and webpack 5, so `@moq/watch`
and `@moq/publish`, which import it at runtime, load in Next.js and in a
server-side import. The Node load check covers every published JS package
built with plain `tsc`, so the next new package cannot ship the same defect.

## Plan

Found by the 2026-11-17 pre-cut correctness audit; reproduced:
`cd js/video && bun run build && node -e 'import("./dist/index.js")'` fails
with `ERR_MODULE_NOT_FOUND` for `dist/presentation`. Six relative imports
lack an extension (`js/video/src/index.ts`, `renderer.ts`, `canvas2d.ts`,
`webgpu.ts`), and `tsc` only rewrites imports that end in `.ts`.
[moq-dev/moq#8914](https://github.com/moq-dev/moq/pull/8914) fixed the same
defect in hang and msf, but its Node check (`js/justfile`,
`sh/js/workspace.py`) lists only those two packages, and the watch/publish
load check runs under Bun, which accepts the imports and resolves
`@moq/video` to its source.

Recommended: add `.ts` to the six imports, and derive the Node load check's
list from the workspace (every published package whose build is plain
`tsc`) instead of naming packages.

Public API: none. Wire: none.

Caused by, for the fix PR's `## Caused by`:

- [moq-dev/moq#5138](https://github.com/moq-dev/moq/pull/5138) - added `@moq/video` with extensionless imports (found by the audit; the check from #8914 missed it)
