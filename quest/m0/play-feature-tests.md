# [M] moq-cli: nothing in the merge gate runs the play tests

## Goal

A broken test in `rs/moq-cli/src/play.rs` fails the pull request that broke it.
Today it fails nothing a reviewer sees, and the module's tests report green
locally without ever being compiled.

## Plan

`play` is off by default in `rs/moq-cli/Cargo.toml`, and `mod play` is
`#[cfg(feature = "play")]` in `main.rs`, so the whole module (its `mod tests`
included) is compiled out of every default build. `just check`, `just test`,
and check.yml are default features only, which means the merge gate never sees
these tests, and neither does a developer running `just test -p moq-cli` after
editing the file.

The one thing that runs them is nightly's `just rs features`, whose last step
is `nextest run --locked --workspace --all-targets --all-features`. That is
post-merge, once a day, and sequential behind a `--no-default-features`
workspace check and an `--all-features` clippy, either of which failing means
the tests never run at all. [uring all-features](/quest/m0/uring-all-features-build.md)
is that exact failure on dev.

So a test that cannot pass reads green through review. #3381 shipped one; a
reviewer caught it, not the gate.

### Shape

Most of what the tests cover does not need the device stacks. `Playback`,
`AudioTimeline`, `fit`, and `Clock` are plain logic, and
`subscribe_waits_for_the_announcement` needs only `moq-net`. Split the module
so that logic compiles unconditionally and only the winit/wgpu/cpal event loop
stays behind `play`. The default `just check` and `just test` then cover the
tests with no new dependency, no new CI job, and no new minutes.

The alternative is naming `--features moq-cli/play` in the per-PR test recipe
the way `just rs windows` names it for its compile. That buys a wgpu + winit +
cpal build in the test job for a handful of tests, so take it only for whatever
genuinely cannot move.

`capture` and `transcode` are gated the same way and have no tests today, so
nothing is silently green there yet. Whatever lands here should make the
default-compiled side the obvious place to put them.

## Related

- [Go smoke client](/quest/m0/smoke-go-client.md) - the other place where CI exercises none of the code
- [uring all-features](/quest/m0/uring-all-features-build.md) - the nightly gate that is the only backstop here, failing on dev
