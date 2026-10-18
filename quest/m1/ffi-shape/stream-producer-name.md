# [S] Name the container stream importer by role

## Goal

The maintainer decides whether the import-side
`MoqMediaContainerStreamProducer` (`media.ContainerStreamProducer` in the
wrappers) is named by role, and any rename lands before the binding release
that the codec namespace reshape gates, so the bindings break once.

## Plan

Still open from #5356, which renamed the per-track consumers to
`media.TrackConsumer` and `media.GroupConsumer`. "Container" here means an
importer that recovers framing from a raw byte stream and may publish several
tracks, unlike those per-track consumers. The rejected crate split (#5460)
had owned this review.

Ask with 2-3 candidate names and a recommendation. A rename touches moq-ffi,
every wrapper, `cpp/moq`'s alias list, the `doc/lib` samples, and the
`## Unreleased` of each affected `MIGRATE.md`; keeping the name deletes this
quest.
