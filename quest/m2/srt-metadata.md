# [S] SRT metadata parity

## Goal

The SRT publisher preserves ID3, SCTE-35, private sections, private PES, and
unknown elementary streams with the same typed `mpegts` catalog and
byte-faithful tracks as `moq-cli publish --container ts`.

## Plan

Replace the media-only catalog used by `moq-srt::Publisher` with the same
MPEG-TS catalog extension path as the CLI importer. Do not special-case only
SCTE-35: PMT stream type, PID, descriptors, section versus PES framing, and
verbatim payloads survive together so future metadata does not need another
gateway patch.

Use shared importer tests for CLI and SRT rather than duplicating a second
metadata implementation. Land the importer change and fixtures here; the
release and moq.pro's (downstream) embedded gateway stay out of this quest.

## Related

- [SEI sidecars](/quest/m2/sei/README.md) - independently separates codec
  metadata after the gateway preserves it
- [ID3 section](/quest/m2/id3.md) - independently gives one preserved metadata
  type a container-neutral catalog contract
