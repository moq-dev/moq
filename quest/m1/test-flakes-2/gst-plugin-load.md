# [S] Interop loads the local moq-gst plugin

## Goal

The interop `gst` cells find the locally built moq-gst plugin on every run,
instead of sometimes failing with "moqsrc not exposed" (the check near
`test/interop/interop.sh:780`), fixed at its cause.

## Plan

Seen in [moq-dev/moq#6745](https://github.com/moq-dev/moq/pull/6745)'s
`just test interop --all` runs: one of two runs failed to load the plugin, and
the other passed. Find whether the plugin build, the registry cache
(`GST_REGISTRY`), or `GST_PLUGIN_PATH` races between cells, and make the
harness load the plugin deterministically.

Public API: none. Wire: none.
