# m0: immediate priorities

## Goal

Settle the public contracts of `moq-archive`, `moq-e2ee`, `moq-sock`,
`moq-uring`, `moq-audio`, `moq-video`, `moq-transcode`, and `moq-nvenc`
before the imminent release, while supplying the reusable GPU media support
needed to remove raw-pixel CPU transfers from the Pronto CARLA demo. These are
independent immediate tracks rather than mutual prerequisites. Then cut the
release moq.pro adopts from the merged tree.

## Plan

dev landed on main as #3793 on 2026-09-20;
[Release](/quest/m0/release.md) names what gates the release that follows.
Published API or wire breaks still land on dev; the quest's Plan says so.

The archive, E2EE, and uring crates are 0.0.x; socket, audio, video,
transcode, and nvenc are 0.1.x so dependents can take compatible patches. Their
API quests gate the release; a published break to a 0.1.x crate targets dev.
Inspect transitive public exposure before changing a shared symbol: `moq-tokio`
publicly re-exports `moq-sock`'s bind module. Keep that re-export and its
current names.

Keep the useful boundaries: archive owns storage and codecs, E2EE owns
protection rather than catalogs, sock owns runtime-neutral sockets, and uring
owns the local worker and its I/O. Prefer standard Rust ranges to a new public
range type. Keep uring's root `Config` reachable, since worker is private;
renaming `TxBuf` or moving bind names does not improve an ownership contract.

Their package boundaries are explicit:

- `moq-archive` replaces reversed integer-pair bounds with validated finite
  `RangeInclusive<u64>` values, unifies streaming and paginated listing under
  archive-owned query/entry types, and hides path helpers that are not consumer
  APIs. Persisted object paths and bytes remain unchanged.
- `moq-e2ee` replaces raw/profile-global construction with application-owned
  secrets and epoch-scoped `Credential`, `Generation`, `Epoch`, and track/group
  handles. Raw crypto, catalog policy, retransmission internals, and the global
  `Publication` registry leave the public surface.
- `moq-sock` makes incomplete reuseport groups unservable and retains every
  member socket for the served group's lifetime.
- `moq-uring` derives worker and steering identity from owned sockets and
  connections instead of independently supplied handles or shard values.
- Published `moq-tokio` keeps its worker signatures and `bind` re-export while
  adapting internal plumbing. Its root names do not move.

The media crates are 0.1.x too, so a published break to them targets dev.
Adapt callers in other packages without breaking their published APIs, C
layouts, or wire formats. Do not bump versions as part of these quests.

Their package boundaries are explicit:

- `moq-audio` owns the PCM/layout and codec configuration split, decoder entry
  point, publication authority, and extensible audio frame and packet
  construction.
- `moq-video` owns frame conversion and construction, decoder output policy,
  synchronous codec thread confinement, capture timestamps and rational rates,
  extensible group configuration and `cut` naming, and its feature defaults.
- `moq-transcode` adopts the video rate, group, output, and feature contracts in
  its public configuration and observations without adding another media model.
- `moq-nvenc` narrows its safe facade around owned resources and completion,
  while loading and incompatibility become fallible public errors.
- Published `moq-mux` gains only the additive shared `rate` namespace. Published
  `moq-ffi`, `libmoq`, and language-binding signatures, layouts, and sentinel
  behavior remain unchanged while their internals adapt.

The agreed media direction is small, honest APIs: typed PCM layouts, rational
video rates, extensible GOP and frame records, explicit ownership, and no knobs
that claim behavior they do not provide. Synchronous codecs remain public and
thread-confined; async sinks own codec execution. Native versus CPU output is a
choice, not a promise that every native backend yields a GPU surface. OpenH264
becomes optional but stays enabled by default. Rendering becomes opt-in;
inexpensive native codec defaults remain.

The Pronto GPU quests remain an independent deliverable within m0. They define
portable Vulkan/CUDA ownership, safe partial NVENC initialization, and a strict
GPU conversion path without changing either API audit. Product integration and
installation live in moq.pro.

Each implementation updates its existing README, examples, and affected docs
inline and adds regression coverage to normal or nightly CI. Feature checks
exercise each media crate independently, since workspace feature unification
hides missing gates. Cross-platform compilation and hardware execution are
separate evidence. The audits were source-based, not a cryptographic review,
fresh compilation, benchmark, or Linux runtime validation.

API-preserving implementation, codec additions, allocation work, and hardware
proof remain in the existing backlog. Keep audio's integrated packetizing
Producer and transcode's validated Ladder and coalescing active cursor. Keep
one video Frame/Surface hierarchy and its deliberate native/wgpu type interop;
do not add another media abstraction or a renderer crate during stabilization.

## Quests

- [Release](/quest/m0/release.md) - the release moq.pro adopts: binding docs, an upgrade page, and a staging soak gate it rather than the merge
- [Audio jitter target](/quest/m0/audio-jitter-target/README.md) - the audio playout target is a measured estimate of arrival timing in both languages, not a round-trip guess
- [A/V clock](/quest/m0/plan-av-clock.md) - the audio playhead drives Sync.reference while audio plays, through per-track sync handles
- [SD rendition for bbb](/quest/m0/bbb-sd.md) - `just pub bbb` publishes a pre-encoded 360p rung beside the 720p source, for localhost demos and moq.pro's fleet demo

## Related

- [Pronto GPU integration](https://github.com/moq-dev/moq.pro/tree/main/quest/main/pronto/gpu) - CARLA bridge, release adoption and desktop installation
