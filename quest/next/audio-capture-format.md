# [S] Validate audio capture overrides before opening devices

## Goal

Unsupported sample-rate/channel overrides fail with the requested format and
device context, and channel counts never wrap while narrowing to the device API.

## Plan

capture::resolve currently overwrites the device default and casts u32 channels
to u16. A value such as 65537 becomes one. Supported ranges are already available
to device enumeration. Validate requested overrides and checked conversions
before opening the stream; make the existing documentation's hint/requirement
wording match the actual contract. Do not silently substitute another format.

Test unsupported rates, 65537 channels, device ranges, valid default selection,
and backend open failures using fake capabilities in CI. No new capture API,
mixing, crop support, or device backend is required.

Public API and wire: unchanged; previously malformed requests are refused.

## Related

- [Capture ergonomics](/quest/future/capture-ergonomics.md) - crop and mixing planning remain independent
