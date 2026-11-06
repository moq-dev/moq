# [S] Container export ends cleanly when its broadcast closes

## Goal

An fMP4 or MP4 export whose broadcast instance closes returns `None` from
`poll_next`, as its docs promise ("`None` once the broadcast ends"), after
the media it already read, instead of failing with the dropped track's error.
A broadcast that fails while it stays live still fails the export.

## Plan

Decided 2026-11-06 (maintainer, on moq-dev/studio's /quest-plan): m3, since
the studio is a named consumer that is not urgent.

`mp4::Export` (`rs/moq-mux/src/container/mp4.rs`) ends when the fMP4 export
under it (`container/fmp4/export.rs`) does, so fix it there, for both.
Tell a closing instance apart from a failure by what the broadcast reports,
not by a timeout or by matching an error string.

Consumer: the studio's per-guest recorder
([moq-dev/studio#65](https://github.com/moq-dev/studio/pull/65)) treats an
export error as the guest leaving only if the broadcast already reads as
closed (`is_closed` in `rs/moq-studio/src/record.rs`). A departure whose
track error arrives before the close then marks the recording failed. The
studio drops that check when it next bumps `moq`.
