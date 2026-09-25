# [S] A failed NVENC encode does not hang shutdown

## Goal

After NVENC rejects an encode (P7 with high-quality tuning returns
`InvalidParam`), the process shuts down promptly. Today it hangs on exit.

## Plan

Reproduce it first with the `encode-presets` example from #4099. Suspects,
from reading the code: `encode::Sink` runs NVENC on the `moq-video-encode`
thread, and `Worker::drop` joins it. That thread then drops the encoder, and
`Session::drop` calls a synchronous end-of-stream `encode_picture`. A
`Pending` that did not finish runs a blocking `lock_bitstream`. Either one can
wedge on a session the driver already refused, and the join then waits
forever.

Fix the cause, for example by skipping end-of-stream on a session whose
encode failed. A timeout on the join doesn't count as a fix. Add a regression
test that forces the failing configuration on hardware where NVENC exists and
asserts teardown returns. Wire it into the nightly GPU lane if there is one;
otherwise say where it runs.

## Related

- [NVENC recovery](/quest/m1/nvenc-recovery.md) - the other NVENC failure path, rate changes and partial init
