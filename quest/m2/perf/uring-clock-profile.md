# [XS] Profile the io_uring clock read

## Goal

A `perf` profile on `moq-quic` measures whether the per-turn clock read from
[#5926](https://github.com/moq-dev/moq/pull/5926) cut the io_uring relay's
`[vdso]` share from #3122's ~2.5 to 3% to the tokio path's ~0.7%, and
whether relay CPU fell with it.

## Plan

#5926 counted `clock_gettime` instead (44.7 to 1.05 calls per turn), since
`perf` was unavailable on its host and `just bench` was too noisy there to
read. That count is a proxy; this quest records the measurement it stood in
for.

Run on a quiet Linux host where `perf` works. Profile the io_uring workers
on the video and chat workloads at #5926's base (`1dd9b83d1`) and at `main`,
and read the `[vdso]` share with `just bench-profile cpu` on
`io-uring-workers`, given the base revision. Read relay CPU from the
`io-uring-workers` rows of `just bench-runtime`, run at both revisions on the
same host and worker count. `just bench BASE` won't do: it runs only the
default tokio runtime.

If `[vdso]` stays well above the tokio share, record what still reads the
clock; a follow-up decides whether it is worth chasing. Either result
deletes this quest.

Public API: none. Wire: none.
