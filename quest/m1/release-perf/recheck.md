# [M] Re-check

## Goal

Every screen-only move in the [epic](/quest/m1/release-perf/README.md) is
re-measured with instruction counts. Each one that holds is fixed in this
quest's PR, one commit per fix, or accepted with a reason recorded in the
epic.

## Plan

Use the instruction-count method from the [performance audit](/quest/m1/audit-perf.md).
Wall time on a shared host can't tell these apart. Build each bench at
`origin/release` and at main, and compare instructions per iteration. A
change that needs a maintainer decision, or is too large to review alongside
the rest, becomes a follow-up through `/quest-plan`.

Leads from reading code on 2026-10-21, strongest first:

1. moq-mux `container-export/{ts,mkv}/4x1` (4.7x, 3.9x). The worst ratio is at
   one frame per track, so look for a fixed cost per export or per track. The
   reap cost from [#5942](https://github.com/moq-dev/moq/pull/5942) that the
   audit found grows with buffered groups instead. Suspects: every export now
   goes through `jitter::Buffer`
   ([#4645](https://github.com/moq-dev/moq/pull/4645),
   [#5309](https://github.com/moq-dev/moq/pull/5309)), and `container/source.rs`
   start-up ([#5947](https://github.com/moq-dev/moq/pull/5947),
   [#5854](https://github.com/moq-dev/moq/pull/5854)). If the jitter path
   dominates, skip it when the delay is zero.
2. `annexb-import` (median 2.1x). Check `group_*` and `append_group` first. If
   they regressed by the same factor, the cost is per group in moq-net
   ([#5803](https://github.com/moq-dev/moq/pull/5803) rewind check,
   [#5725](https://github.com/moq-dev/moq/pull/5725) largest notifications,
   [#6020](https://github.com/moq-dev/moq/pull/6020)), not in the importer.
3. `track_stamp_groups` (median 2.7x). The first frame on a timed track now
   takes the track lock and `bounds`, `starts`, and `sequence` mutexes
   ([#5803](https://github.com/moq-dev/moq/pull/5803),
   [#6074](https://github.com/moq-dev/moq/pull/6074)). Pass the checked start
   into `first_frame`, merge `bounds` and `starts` under one lock, and skip
   `advance_largest`'s write and wake when nothing changed.
4. `track_datagram_subscriber_join` (median 2.3x). `Cursor::new` scans the
   datagram ring twice ([#5195](https://github.com/moq-dev/moq/pull/5195)).
   Track the newest datagram timestamp on push. The window's construction cost
   from [#5929](https://github.com/moq-dev/moq/pull/5929) was cut in
   [#6112](https://github.com/moq-dev/moq/pull/6112), and the rest belongs to
   [Allocation-free single-range subscriber window](/quest/m1/window-alloc.md).
5. `track_parallel_write`, `track_fanout_frame`, `track_empty_run`,
   `group_write_frames` (1.2 to 1.6x). This is constant per-group and
   per-frame cost from several features. Resolve `now` once per poll in
   `Drift`, and skip no-op `advance_largest` writes. Accept the rest.
6. `origin/announce_duplicate/30d_1s` (4.3x). Compare against `1d_1s`, to
   tell a constant cost per change from a cost per duplicate. Leads: an
   epochless winner switch is now a `Restart`
   ([#5087](https://github.com/moq-dev/moq/pull/5087),
   [#5443](https://github.com/moq-dev/moq/pull/5443),
   [#4942](https://github.com/moq-dev/moq/pull/4942),
   [#5144](https://github.com/moq-dev/moq/pull/5144)).
7. `session_delivery_{relays,paced,scale}` (1.4 to 1.6x). Expected to be the
   per-group cost above plus the flow-controlled mock transport
   ([#5530](https://github.com/moq-dev/moq/pull/5530)). Re-measure after the
   fixes above, and accept what remains.
8. moq-stats `aggregate` at live=16, departed=4096, and kio
   `contended_{notify,log}` at 16 threads: confirm they are noise. If the
   aggregate row holds, suspect [#4874](https://github.com/moq-dev/moq/pull/4874)
   and [#4904](https://github.com/moq-dev/moq/pull/4904).

Add a bench sweep wherever a fix removes a slope, so the slope stays visible.


### Recheck evidence, 2026-10-22

[moq-dev/moq#6294](https://github.com/moq-dev/moq/pull/6294) adds stable timed
profiling boundaries to the existing mux benchmarks. `container-export` again
opens and boxes each exporter in setup, matching release's drain boundary.
This changes main's instruction counts by less than 1% at `ts/4x1` and
`mkv/4x1`, so it is a comparison correction, not the explanation for the gap.
No production behavior, public API, compression level, wire shape, or jitter
policy changes are proposed.

The samples below use Callgrind 3.27.1 and Rust 1.98.1. Each count collects only
one `--bench --test --exact <id>` timed helper invocation, excluding fixture
setup and teardown. Main's base is `f01e847b8b6c7bf9de802916a95e549848279b98`;
release is `36f392f492f571454e5abc0e441ddc0168f16477`.

Two comparisons are kept separate:

- Branch-default codegen and pinned dev shells: main uses fat LTO, one codegen
  unit, and glibc 2.44; release uses its default codegen and glibc 2.42. Both
  builds retain symbols and line tables. The original four-track, one-frame
  ratios are 1.43x TS and 1.33x MKV in instructions, rather than the screen's
  4.7x and 3.9x wall-time ratios. The instruction gap remains real.
- Controlled source attribution: release is rebuilt under main's dev shell,
  with fat LTO and one codegen unit, and both branches use the opened-drain
  boundary. The binary's RUNPATH confirms glibc 2.44. This intentionally
  excludes artifact differences and does not replace the default-profile
  comparison above.

Export instruction counts, with tracks and frames varied independently:

| Case | Original main | Native release | Aligned main | Common-toolchain release | Aligned/common ratio |
| --- | ---: | ---: | ---: | ---: | ---: |
| `ts/1x1` | 200,253 | 149,683 | 198,185 | 122,929 | 1.61x |
| `ts/4x1` | 594,659 | 415,755 | 592,399 | 348,489 | 1.70x |
| `ts/16x1` | 2,383,409 | 1,628,060 | 2,385,114 | 1,389,655 | 1.72x |
| `ts/4x16` | 1,622,912 | 967,615 | 1,620,901 | 819,262 | 1.98x |
| `ts/4x64` | 4,940,787 | 2,797,743 | 4,938,520 | 2,367,962 | 2.09x |
| `mkv/1x1` | 194,317 | 156,790 | 192,522 | 129,266 | 1.49x |
| `mkv/4x1` | 585,677 | 439,619 | 584,858 | 368,554 | 1.59x |
| `mkv/16x1` | 2,466,999 | 1,719,238 | 2,490,278 | 1,451,270 | 1.72x |
| `mkv/4x16` | 1,177,426 | 900,427 | 1,172,743 | 752,759 | 1.56x |
| `mkv/4x64` | 3,086,910 | 2,394,090 | 3,068,491 | 2,002,533 | 1.53x |
| `flv/4x1` | 523,965 | 390,054 | 523,031 | 322,160 | 1.62x |

Annex B import instruction counts, with frames and payload size varied
independently. The `bytes` and `frames` benchmark names run the same import;
only their throughput units differ, so the table counts the `frames` variants.

| Frames | Payload bytes | Main | Native release | Common-toolchain release | Main/common ratio |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 256 | 2,588,890 | 664,198 | 652,692 | 3.97x |
| 1 | 4096 | 2,596,696 | 672,686 | 659,361 | 3.94x |
| 16 | 256 | 2,932,920 | 993,269 | 921,061 | 3.18x |
| 16 | 4096 | 3,077,962 | 1,120,895 | 1,044,306 | 2.95x |
| 64 | 256 | 5,613,684 | 4,017,621 | 3,755,359 | 1.49x |
| 64 | 4096 | 6,197,108 | 4,542,332 | 4,241,196 | 1.46x |

The one-frame 256-byte main import spends 2,099,906 instructions (81%) in
`memset`. Its callers initialize cold DEFLATE state for catalog and timeline
writes. `Outputs::emit` accounts for 1,969,749 inclusive instructions (76%);
these inclusive costs overlap and must not be added together. Snapshot and
window encoders already start with no DEFLATE state, constructing it when
writing their first compressed value. An unused eager stream encoder exists
elsewhere but is not this measured path.

Main has per-track timelines and immediately completed catalog records; release
uses the older shared broadcast timeline. The new enrollment and first media
duration declaration produce additional catalog/timeline work. The import
ratio falling from 3.97x at one frame to 1.49x at 64 frames (256-byte payloads)
points to a large fixed metadata cost, rather than only the per-group lead.
This identifies necessary output work, not permission to disable it or to
accept its cost without a decision.

Repeating the aligned main `ts/4x1` collected 592,589 instructions, 0.032%
from the first sample. Repeating the one-frame 256-byte import collected
2,590,889 instructions, 0.077% from its first sample.

The corrected benchmark runs all 99 existing mux import/export cases in test
mode, validating their output. `just rs check -p moq-mux` passes all-target
clippy and lint; the complete mux suite passes 1,602 tests with five existing
nightly skips. `just ci` passes Rust, quest, and drill checks, then the host's
unchanged VM-image build fails at `unshare` writing `/proc/self/uid_map`.
Later dispatcher phases do not run after that failure. The native release profiler's first startup
stalled in its old environment; the successful comparisons all use one exact
main-pinned Callgrind executable with debuginfod lookup disabled. Counts from
the failed startup are not used.

A runner discovery gap belongs to the comparison-harness quest: changing the
Nix shell alone left Cargo considering an old-libc binary fresh. A separate
fresh target and source timestamps forced the controlled rebuild; RUNPATH
verified the result. The shared runner is unchanged here.

### Remaining work and decisions

- The measured mux gaps remain unresolved. Recommend retaining compressed
  catalogs, per-track timeline publication, and zero-delay jitter behavior.
  Decide whether to accept their identified fixed feature cost after the
  remaining attribution, or investigate a behavior-preserving optimization.
  No feature cost has been accepted in the epic by this draft.
- TS export still grows to a 2.09x controlled ratio at `4x64`. Existing
  [abort reaping](https://github.com/moq-dev/moq/pull/6281),
  [redundant failover](https://github.com/moq-dev/moq/pull/6113),
  [TS routing](https://github.com/moq-dev/moq/pull/6252), and
  [PMT export](https://github.com/moq-dev/moq/pull/6289) own overlapping code;
  their source changes are not duplicated here.
- The track/group, origin duplicate, session delivery, stats aggregate, and
  contended kio leads above still need their instruction-count rechecks. Live
  resume/track and stats work also owns some of those paths. The original
  leads remain active; this draft does not complete or retire the quest.
