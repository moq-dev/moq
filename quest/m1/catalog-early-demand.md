# [XS] Guard video demand before the first keyframe

## Goal

A regression test proves that demand on a video track before its first
keyframe does not end the import or modify an unpublished rendition.
It guards the importer failure reported in #4945.

## Plan

The enabled-flag change in [#4915](https://github.com/moq-dev/moq/pull/4915)
removes `publish_stalled` and the importer detector hooks that called
`track.modify()` before a configuration was published. The catalog now
publishes a resolved configuration through `track.set()`; it has no
mutation driven by early demand. The relay half of the report is fixed by
[#4942](https://github.com/moq-dev/moq/pull/4942)'s publisher epochs.

The source fixes are present; the before-first-keyframe regression from
the imported issue remains to be added. Exercise a video importer with
track demand before it resolves its configuration, then deliver its first
keyframe and require the import to continue. Use mocked time if needed,
and keep the test in the existing moq-mux Check/Test suite. Do not restore
the removed detector or add an availability mechanism.

Public API: none. Wire: none.

## Closes

- [#4945](https://github.com/moq-dev/moq/issues/4945) - early video demand does not end a replacement import with "rendition is not published"
