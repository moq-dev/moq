# [S] Publish loads mediabunny only for file sources

## Goal

A `<moq-publish>` element that captures a camera or screen no longer
downloads mediabunny. Today `js/publish/src/element.ts` statically imports
the sources, and `source/file.ts` imports `ALL_FORMATS` from mediabunny, so
every publish element pays about 99 KB gzip (379 KB minified). Of the element's 218 KB
gzip first load, that is the largest avoidable piece.

## Plan

Decided in planning:

- Load the file source with a dynamic `import()` when a file source is
  selected.
- Keep `ALL_FORMATS`, so any container mediabunny reads still works. Leave
  mediabunny bundled into publish's dist rather than making it external.

Guidance:

- The public `@moq/publish` exports can still expose the file source
  statically. Only the element and any default-source path need to stop
  reaching it eagerly.
- Verify with a bundler metafile that the camera-only element no longer
  contains mediabunny, and that picking a file still works in the demo.

## Related

- [Size report](/quest/m1/size-report.md) - tracks the publish element's first-load size
