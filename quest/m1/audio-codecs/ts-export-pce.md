# [S] TS export carries an AAC track's program config element

## Goal

`moq-mux`'s MPEG-TS export writes an AAC track described by a program config
element (channelConfiguration 0) as ADTS with channel_config 0 and the PCE at
the start of the first raw data block, so a TS round trip keeps the layout.
Today the export derives the ADTS channel_config from the channel count, which
mislabels such a track.

## Plan

Take the PCE from the track's AudioSpecificConfig description with the parser
that reads it on import, and write it as ffmpeg does: once, leading the first
raw data block. Tracks with a nonzero channelConfiguration are unchanged. Test
a round trip of the quad fixture from the PCE import (`aac_quad.ts`): import,
export, and import again, asserting the same description and channel count.
