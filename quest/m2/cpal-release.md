# [XS] cpal release with allocation-free error emission

## Goal

A published cpal release emits stream errors from its real-time paths without
allocating, so `moq-audio` can pin it and stop paying an allocator call on
the audio thread when a device fails.

## Plan

Propose the change upstream: a `&'static str` message on the RT emission
paths, or deferring the `format!` in `From<coreaudio::Error>` (and the other
hosts' equivalents) to a non-RT thread. Record the maintainers' answer here.
When a release carries it, remove the bullet below, bump the pin, and
complete this quest; that unblocks the dependent one.

## Required

- A cpal release that emits errors from its real-time paths without allocating

## Related

- [#3247](/quest/m2/3247-moq-audio-cpal-allocates-on-the-audio-thread.md) - the moq-audio work that waits on this release
