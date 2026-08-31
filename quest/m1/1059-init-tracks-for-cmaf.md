# [M] Init tracks for CMAF

## Goal

Implement and verify the behavior tracked in [#1059](https://github.com/moq-dev/moq/issues/1059)
within the issue's stated scope and boundaries.

## Plan

Still real: the CMAF init segment is base64-embedded in the catalog on both
branches. This is a catalog-format change across rs/hang, js/hang, moq-mux,
moq-hls, and draft-lcurley-moq-hang; land it on dev.

### Issue context

I was trying to be smart and put everything in the catalog, but there's a lot of issues with this approach. We should go back to a track per init segment.

## Closes

- [#1059](https://github.com/moq-dev/moq/issues/1059) - close this issue when the quest finishes
