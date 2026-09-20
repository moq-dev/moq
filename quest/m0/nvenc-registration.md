# [S] Roll back failed NVENC input registration

## Goal

An NVENC resource mapping failure releases the registration and preserves the
input allocation's lifetime. Failed setup cannot leave a registered resource
behind or prevent subsequent encoder work from reclaiming its resources.

## Plan

- Reproduce the partial-initialization failure in
  rs/moq-nvenc/src/safe/buffer.rs: register_generic_resource registers the
  resource, then returns on a mapping error before constructing the cleanup
  guard. The successful path already unmaps and unregisters on drop.
- Make registration and mapping transactional, retaining the input owner
  through cleanup. Preserve the original failure and surface any cleanup
  failure without losing ownership silently.
- Add an injectable failure regression through the normal test commands:
  registration succeeds, mapping fails, unregister runs exactly once and the
  input owner remains valid until cleanup finishes. Also check successful
  mapping and destruction so rollback does not introduce double cleanup.

## Related

- [GPU conversion and NVENC](/quest/m0/video-gpu-encode.md) - the strict GPU path relies on safe partial initialization
