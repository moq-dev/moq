# [XS] moq-mux debounce test on the paused clock

## Goal

moq-mux `container::ts::export_test::debounce_opens_without_a_media_clock`
advances on the paused clock and sleeps no real time.

## Plan

It died with SIGTERM once in a combined run. It is marked
`start_paused = true` but sleeps 1.2 s of real time, because the debounce
reads `crate::Clock`, which uses `std::time::Instant`, so the paused tokio
clock never reaches it. Let the test drive `crate::Clock`'s time (a tokio
`Instant` under test, or an injected source) so the window passes on the
paused clock, and drop the real sleep.
