# m0: immediate media

## Goal

Settle the public contracts of `moq-audio`, `moq-video`, `moq-transcode`, and
`moq-nvenc` before their first 0.1 releases, while supplying the reusable GPU
media support needed to remove raw-pixel CPU transfers from the Pronto CARLA
demo on its Linux/NVIDIA desktop. Product integration and installation live in
moq.pro.

## Plan

The Pronto GPU quests remain an independent deliverable within m0. They define
portable Vulkan/CUDA ownership, safe partial NVENC initialization, and a strict
GPU conversion path without changing the scope of the pre-0.1 API audit.

The four audited crates are 0.0.x, so their changes target main. Adapt callers
in other packages without breaking their published APIs, C layouts, or wire
formats. Do not bump versions as part of these quests. The final review records
when the four crates are ready for a separately requested release.

The package boundaries are explicit:

- `moq-audio` owns the PCM/layout and codec configuration split, decoder entry
  point, publication authority, FEC removal, AEC attachment, playback outcome,
  and extensible audio frame and packet construction.
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

The agreed direction is small, honest APIs: typed PCM layouts, rational video
rates, extensible GOP and frame records, explicit ownership, and no knobs that
claim behavior they do not provide. Synchronous codecs remain public and
thread-confined; async sinks own codec execution. Native versus CPU output is a
choice, not a promise that every native backend yields a GPU surface.

OpenH264 becomes optional but stays enabled by default. Rendering becomes
opt-in; inexpensive native codec defaults remain. Hardware libraries already
loaded at runtime do not need a toolkit dependency added to the build.

Each quest updates the existing crate documentation, examples, and affected
callers with its change. Contract tests run in CI; feature checks exercise each
crate independently, since workspace feature unification hides missing gates.
Cross-platform compilation and hardware execution are separate evidence.

The audit was source inspection at `b2ef453e67`, not fresh compilation or
benchmarking. All public module families and their in-tree consumers were
reviewed; the generated NVIDIA ABI was inspected only where wrapper safety
depended on it. Performance concerns remain hypotheses until measured.

API-preserving implementation, codec additions, allocation work, and hardware
proof remain in the existing backlog. Keep audio's integrated packetizing
Producer and transcode's validated Ladder and coalescing active cursor. Keep
one video Frame/Surface hierarchy and its deliberate native/wgpu type interop;
do not add another media abstraction or a renderer crate during stabilization.
Preserve the documented ability to abort publication after finish unless an
actual ownership fix requires a replacement error-finalization contract.

## Quests

- [Vulkan/CUDA surfaces](/quest/m0/video-vulkan-cuda.md) - retain producer slots
  and synchronize GPU access safely across Vulkan and CUDA
- [NVENC registration rollback](/quest/m0/nvenc-registration.md) - release resources when mapping fails after registration
- [GPU conversion and NVENC](/quest/m0/video-gpu-encode.md) - convert, resize and
  encode imported frames without CPU pixel transfers or fallback
- [NVENC resources](/quest/m0/nvenc-resources.md) - a small safe facade retains submitted resources through completion
- [NVENC loading](/quest/m0/nvenc-loading.md) - unavailable or incompatible drivers return errors instead of panicking
- [Codec threads](/quest/m0/video-thread-ownership.md) - synchronous codec handles cannot escape their owning thread
- [Media features](/quest/m0/media-features.md) - OpenH264 can be excluded, rendering is opt-in, and feature aliases disappear
- [Shared rate policy](/quest/m0/media-rate-policy.md) - the planned public namespace move happens before 0.1
- [Audio configuration](/quest/m0/audio-config.md) - PCM layout, codec settings, and subscription policy have distinct contracts
- [Audio publication](/quest/m0/audio-publication.md) - callers get demand authority and supported options, not internal transport or resampler machinery
- [Remove ineffective FEC](/quest/m0/audio-fec.md) - no public flag promises redundancy the encoder never emits
- [AEC ownership](/quest/m0/audio-aec.md) - one microphone owns an adaptive canceller and controls remain shareable
- [Playback outcome](/quest/m0/audio-playback.md) - nonblocking writes report accepted and dropped audio
- [Video output](/quest/m0/video-output.md) - codec output and subscription policy are separate, with native or CPU frames
- [Video frames](/quest/m0/video-frames.md) - conversions preserve typed pixels and frame records can grow
- [Video timing](/quest/m0/video-timing.md) - capture time and fractional frame rates survive capture, encoding, and transcode
- [Video GOP](/quest/m0/video-gop.md) - the group contract is extensible before intra-refresh implementation
- [Media release review](/quest/m0/media-release-review.md) - verify the settled contracts before separately authorizing 0.1 releases

## Related

- [Pronto GPU integration](https://github.com/moq-dev/moq.pro/tree/main/quest/m0/pronto/gpu) - CARLA bridge, release adoption and desktop installation
