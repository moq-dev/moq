# [S] Interop's CLI FETCH rust -> rust finds its group

## Goal

`just test interop --all`'s `CLI FETCH rust -> rust` case fetches group 1 of
`tail` on every run, instead of sometimes failing with "group 1 of tail not
found".

## Plan

Seen 2026-11-15 on the branch of
[moq-dev/moq#8836](https://github.com/moq-dev/moq/pull/8836), whose change
touches only lite-07 AUTH; the case runs on `moq-lite-06`, and the next full
run passed. The fetcher's session logged `session error err=no error` and the
publisher's `track::Producer dropped without finish() or abort()` just before
the fetch failed, so the publisher may end the track before the fetch is
served. Reproduce under load (`test/interop/interop.sh`), find whether the
fetch races the publisher's exit or the relay's cache, and fix that cause.

Public API: none. Wire: none.
