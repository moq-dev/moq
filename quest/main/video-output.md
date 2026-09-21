# [M] Separate video decoder output from subscription policy

## Goal

Low-level video decode settings describe the codec and its frame output;
Consumer options describe subscription behavior. Callers choose native or CPU
output without backend-specific GPU booleans or redundant acceleration modes.

## Plan

Config currently combines backend/resize with start/max_age that bare Decoder
ignores. gpu_frames only affects VAAPI, even though other backends return GPU
surfaces regardless; Acceleration::Gpu behaves like Auto and can fall back.

Separate those layers. Establish an extensible native-or-CPU output choice:
native permits the backend's natural representation, including CPU, while CPU
produces the documented typed CPU pixels. Conversion is explicit. Retain the
efficient existing default behavior rather than promising GPU residency or
requiring a download accidentally. Remove the redundant GPU preference.
Keep best-effort decoder scaling explicitly identified as a hint, distinct
from the exact-size frame conversion operation.

Adapt live/fetch transcode paths, rendering, and binding internals. Test config
propagation with CPU and fake-native backends, explicit conversion, a backend
without scaling, and subscription start/max_age separately. Compile the platform
paths through CI and preserve unsupported-backend refusal.

Public API: decoder/consumer configuration and output policy change. Wire and
published binding layouts: unchanged. Update examples and cancellation docs.
