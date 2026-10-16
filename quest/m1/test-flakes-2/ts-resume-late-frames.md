# [M] TS same-instance resume keeps its late frames

## Goal

`the_same_instance_within_the_linger_continues` (`rs/moq-cli/tests/export.rs`)
gives the same verdict on every run of the same code. Its late-frame drops
are either fixed in production or shown to be fixture scheduling and fixed
in the fixture, whichever the evidence says. The strict assertion stays:
the resumed output must contain new media.

## Plan

Found while landing [#5545](https://github.com/moq-dev/moq/pull/5545), which
rebuilt the fixture around one importer across a real relay connection loss,
and seen again on [#5569](https://github.com/moq-dev/moq/pull/5569). In one
loaded run the exporter dropped late frames and the test failed; a later run
passed with no fixture change.

- Reproduce under load first (loop the test beside a loaded `just check`),
  keeping the exporter stderr #5545 now retains, and record which frames
  were dropped and why.
- Decide the cause from that evidence. Production: the exporter or relay
  drops frames a real returning publisher would also lose, so fix it there.
  Fixture: the test feeds the capture tail faster or later than a real
  publisher would, so pace it on the media clock or a mocked clock.
- No retries, wider timeouts, or a looser new-content assertion.
- Compare with [TS decoded content](/quest/m1/test-flakes-2/ts-video-content.md):
  its first hypothesis (a zero `max_delay` live subscription) may cause both.

## Related

- [TS decoded content](/quest/m1/test-flakes-2/ts-video-content.md) - the CI TS compliance flake, possibly the same cause
