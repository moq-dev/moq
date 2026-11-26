# [S] A reannounced sibling instance never replays its finish

## Goal

`moq-gst`'s `source::imp::session_tests::a_reannounced_sibling_instance_does_not_replay_its_finish`
(`rs/moq-gst/src/source/imp.rs`) passes every run, alone and in a loaded full run, fixed at
its cause.

## Plan

Seen on 2026-11-25 in a full local `just check` while landing
[moq-dev/moq#9602](https://github.com/moq-dev/moq/pull/9602), and reproduced there with
that PR's fix disabled, so it predates it. Alone, `just rs test -p moq-gst -E
'test(a_reannounced_sibling_instance_does_not_replay_its_finish)' --stress-count 300`
failed once in 38 and once in 70 iterations, and passed 300 of 300 twice.

It fails the final count: two `Seen::Start` on the pad where one is expected. The recorded
list shows the first instance replaying after its reannounce: `Start`, `Caps`, then
`Segment("video_0", 53000)` and `Buffer("video_0", 1)` again, a second `Start`, and only then
the second instance's `Segment` and `Buffer("video_0", 2)`. So the finished track's group
sometimes plays again when its route comes back, which is the bug the test's name rules out.
Find whether the source re-subscribes the finished track on the route's new `Start` event
before it learns the instance finished, and fix it there.
