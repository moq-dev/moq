# [M] Recover NVENC initialization and rate-change failures

## Goal

NVENC releases every partially initialized resource, destruction does not
panic, and a rejected rate change leaves the last accepted settings intact.

## Plan

Registration can succeed before mapping fails in safe/buffer.rs, and several
destructors call expect on driver cleanup. Session::reconfigure mutates retained
bitrate/VBV fields before the driver accepts the change; a rejected zero-rate
update can corrupt the basis of the next proportional update.

Use the settled resource ownership model and a fake driver to verify rollback
after each acquisition stage, cleanup order, and teardown during an existing
failure. Compute candidate rate settings with checked arithmetic and commit
only on driver success. A following successful update must be based on the
last accepted settings. Preserve fallible explicit operations and non-panicking
Drop without disguising a live operational failure as success.

Run the failure-injection suite in CI. Do not duplicate cleanup guarantees
already delivered by the ownership quest. Public API and wire: unchanged.

## Required

- [NVENC resources](/quest/m0/nvenc-resources.md) - settled lifetime and cleanup ownership
- [NVENC loading](/quest/m0/nvenc-loading.md) - driver failures reach the caller as errors
