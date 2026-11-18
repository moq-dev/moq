# [M] Finalize and repair interrupted MP4 recordings

## Goal

A file-backed caller can durably finish a recording through one library operation. Repair recovers intact media after a torn header write and verifies the movie index before reporting success.

## Plan

With the recording and repair implementation from [moq-dev/moq#7560](https://github.com/moq-dev/moq/pull/7560), writing just eight bytes of the final 16-byte header left five H.264 frames and the index on disk, but repair refused it. Removing just the appended index from a finalized copy made repair report success on a file ffprobe could not open.

Put the reusable file finalization operation at the layer owning the format, with the CLI using it. Test interruption at the writes that change what a new reader sees. Independently check recovered frames with ffprobe, including a second repair.
