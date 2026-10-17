# [XS] Tighten the Opus near-zero browser budgets after the build flush

## Goal

Seven scheduled nightlies on `main` after the harness began flushing its build
before the first row show the Opus `near-zero` `isolated` row holding the
same 60 ms target as the `plain` row with no underrun episodes, and
`test/audio-quality/budgets.json` carries ceilings re-measured from them.

## Plan

Waits on the nightlies: check the `audio-quality-run` artifacts of the
scheduled Nightly runs on `main` (`gh run download <id> -n audio-quality-run`)
once seven have run since the fix merged. Never dispatch the nightly by hand.

The stall was never the isolated ring. That row is simply the matrix's first,
and a cold `cargo build` left about 800 MB dirty when it started. On the ARM
runner each periodic writeback burst (100 to 280 MB, about 10 s apart) froze
the page's main thread for 100 to 320 ms until the backlog drained, about 30 s
in. A branch experiment put the plain ring first after a cold build, and it
failed the same way (540 ms target, the same 10 s long-task clusters).
Isolated-first, with the build flushed beforehand, held 60 ms.

Re-measure the Opus near-zero rows by the rule in the budgets note, and drop
the note's sentence that budgets the isolated row's one-second drops as
measured. If the first row still shows long-task clusters, compare
`<tag>.longtasks.json` against the runner's disk activity before changing the
player.

## Related

- [AAC bursty budgets](/quest/m1/audio-jitter-target/aac-bursty-budgets.md) - the same re-measure for the AAC rows, on its own fix's nightlies
