# [M] Transcode rungs scale off the runtime and keep bounded state

## Goal

moq-transcode resizes frames on its encoder threads, not on tokio workers,
a flapping source size neither re-probes encoders serially nor grows state
forever, and a rung's live and fetch paths stop being two writers to one
track.

## Plan

From the 2026-10-24 pre-cut audit at 151a529670. Source-inferred, partly
speculative until transcode faces untrusted publishers.

- `Frame::resize` (CPU bilinear, plus the CUDA `download_i420` fallback)
  runs inline on the rung task (`rs/moq-transcode/src/rung.rs:395` live,
  `:815` fetch), so sources times rungs times 30 fps pins runtime threads.
  Scale inside `encode::Sink` on the encoder's own thread.
- Each catalog edit to `coded_width`/`coded_height` mints new rung names
  and runs a real encoder `probe()` serially inside `select!`
  (`lib.rs:181`, `pipeline.rs:141`); every new name stays in `active` and in
  every cursor's `seen` map, and `next_update` scans the whole map per wake
  (`active.rs:176-211`, `:347-357`). Memoize rung entries by shape and drop
  retired entries that never encoded a frame (`active::Consumer` stops
  reporting them; document it).
- Live and fetch writing one track forces the duplicate skip, retirement
  draining fetches, and open-group logic (824179a3a1, 1131b80386,
  4ae871c947, 1c7e17f93b). The comment at `rung.rs:369` calls merging them a
  tracked follow-up, but no quest existed; this is it. Merge them, or
  delete the comment with the reason they stay apart.

Public API: `active::Consumer` reports fewer renditions. Wire: none.
