# main: immediate priorities

## Goal

Settle the public contracts of `moq-archive`, `moq-e2ee`, `moq-sock`,
`moq-uring`, `moq-audio`, `moq-video`, `moq-transcode`, and `moq-nvenc`
before the imminent release, while supplying the reusable GPU media support
needed to remove raw-pixel CPU transfers from the Pronto CARLA demo. These are
independent immediate tracks rather than mutual prerequisites.

## Plan

The archive, E2EE, socket, and uring crates are 0.0.1 on main after the dev
merge. Their six API quests gate the release and target main under the 0.0.x
exception. Inspect transitive public exposure before changing a shared symbol:
`moq-tokio` publicly re-exports `moq-sock`'s bind module. Keep that re-export
and its current names.

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

The media crates are also 0.0.x, so their changes target main. Adapt callers in
other packages without breaking their published APIs, C layouts, or wire
formats. Do not bump versions as part of these quests. The media review records
when the four crates are ready for a separately requested 0.1 release.

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

The Pronto GPU quests remain an independent deliverable within main. They define
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

- [GPU conversion and NVENC](/quest/main/video-gpu-encode.md) - validate the GPU
  conversion, resize and NVENC path for imported frames on NVIDIA hardware
- [Media release review](/quest/main/media-release-review.md) - verify the settled contracts before separately authorizing 0.1 releases

## Related

- [Pronto GPU integration](https://github.com/moq-dev/moq.pro/tree/main/quest/main/pronto/gpu) - CARLA bridge, release adoption and desktop installation
