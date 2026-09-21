# [M] Bound renderer retention and validate output resources

## Goal

Video rendering has bounded retained imported surfaces, predictable device-loss
cleanup, and errors for unsupported output configuration through its existing
Result contract.

## Plan

The Linux completion worker uses an unbounded channel and waits indefinitely
for submissions while retaining source leases. Renderer output creation checks
nonzero size but forwards format, usage, and dimensions to wgpu without checking
all supported combinations/device limits. Trace both paths before changing
ownership or validation.

Bound in-flight retention without recycling source pixels before completion;
define cleanup when a device is lost. Validate supported format/usage/size or
route validation failures through the existing error result. Preserve the
documented reusable output-texture alias: a returned handle is overwritten on
the next render, and callers needing independent storage copy it.

Measure per-frame conversion, bind-group/view allocation, and retained memory.
Optimize only demonstrated costs. Test rejected configurations, delayed
completion, teardown, and device-loss handling in CI with fake completion where
possible; keep native import proof in hardware jobs. Public API and wire: none.

## Related

- [Hardware validation](/quest/future/video-hardware.md) - real graphics-device proof
