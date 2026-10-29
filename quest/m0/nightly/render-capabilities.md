# [S] The software renderer tests both capability paths

## Goal

The software-renderer lane verifies output selection with and without DMA-BUF import support without requiring hardware the runner lacks.

## Plan

[The render-sw job](https://github.com/moq-dev/moq/actions/runs/40698329026/job/122851001395) fails in
`the_output_follows_the_device` at "the adapter imports DMA-BUFs".
`just rs render-sw` deliberately uses Mesa's software Vulkan driver, which
lacks `VULKAN_EXTERNAL_MEMORY_DMA_BUF`; the test requires that feature to
open its second device.

Fix the capability test's setup while retaining assertions for both supported
and unsupported devices. Exercise output selection through a private,
render-gated capability seam with the DMA-BUF feature present and absent, so
both assertions run on CI without opening an import-capable device. Production
capabilities still come from the actual device; the seam adds no public API or
caller override. Keep the real rendering and GPU-frame refusal tests
on the software driver. Avoid merely skipping the supported-path assertion on
every CI host, adding a production fallback, or making default video builds
pull in a graphics stack solely for a test.

Public API: none expected. Wire: none expected.
