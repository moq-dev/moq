# Wire integer precision

QUIC varints carry 62 bits; the leading-ones format also carries full `u64`
values. Rust holds these as `u64`. JavaScript holds an unbounded identifier or
counter as `bigint`. A field represented by `number` is an implementation limit:
its decoder refuses values above `Number.MAX_SAFE_INTEGER` before conversion.
`Reader.u53()` and `Cursor.u53()` enforce this for both encodings. Conversions
from an already decoded bigint use `util/bigint.ts`.

The classification below covers the numeric wire fields in `src/lite` and
`src/ietf`, including the model boundary. It changes no field's wire encoding.

| Fields | Representation and reason |
| --- | --- |
| Subscribe/request IDs, joining request IDs, track aliases, FETCH headers, GOAWAY request ID | `bigint`: opaque peer-chosen identities, including map keys and parity checks. |
| Group sequences, absolute anchors, wanted groups, range starts/ends, drops, relative joining offsets | `bigint`: the entire group namespace must be addressable. |
| Relative anchors | `bigint`: counts back through that same namespace; neither the count nor the latest group is bounded by JS array size. |
| IETF subgroup IDs, including explicit FETCH subgroups | `bigint` in the codec. The model supports only subgroup zero and refuses the others without narrowing their identity. |
| IETF datagram Object ID | `bigint` in the codec. The model delivers only Object 0, dropping all others without narrowing their identity. |
| IETF control Locations and LOCATION\_FILTER Group/Object IDs, End-of-Range encoder | `bigint`. A derived frame selection bound clamps to the model's safe frame-count ceiling: no model object exists at or above that count, so either a skip or an exclusive end selects exactly the same content. The peer's Location remains bigint. |
| Lite GROUP Frame Start, FETCH/SUBSCRIBE frame bounds, SUBSCRIBE\_START Largest Frame; IETF subgroup object deltas, FETCH object IDs and End-of-Range decoder | Safe `number`: indices/counts into the JS group's frames, shared with `Group.Consumer.frameCount` and `Track.Position.frame`. `u53` refuses an unsafe peer index; the model also refuses appending past the safe count. Compact IETF subgroup-object mode supports only zero, compared at full width before refusal. |
| Lite frame zigzag timestamp deltas | `bigint` during accumulation. Each resulting absolute timestamp is range-checked before conversion to the numeric `Timestamp`. Negative and unsafe accumulated timestamps fail. |
| Lite datagram timestamps and time anchors; IETF object Timestamp property | Safe `number`: the existing `Timestamp` API uses JS media clocks, allows fractional local times, and explicitly bounds values and scale conversions. Unsafe wire timestamps fail rather than changing their unit or rounding them. |
| TIMESCALE | Positive safe `number`: used by the numeric timestamp/clock conversion API. Unsafe declarations fail. Zero means untimed only on wires that define it. |
| Publisher/subscriber max age, max delay, min delay, IETF FILL\_TIMEOUT, retry interval, GOAWAY timeout, AUTH expiry | Safe numeric milliseconds at the clock/model boundary. Optional offsets are removed in bigint before narrowing where supported. IETF raw parameters/properties remain bigint until consumed. Unsafe waits fail; FILL\_TIMEOUT no longer saturates. Legacy lite publisher-age values at/above the documented unbounded sentinel still mean no limit. |
| Track refresh | `number`, bounded to `u32` by TrackInfo/model validation. |
| PROBE bitrate/RTT and legacy SESSION\_INFO bitrate | Safe `number`: measurements used by numeric bandwidth and clock calculations, with unsafe declarations refused by `u53`. |
| ACTIVE\_COUNT and legacy ANNOUNCE\_PLEASE active count | Safe `number`: decremented against actual announcement messages, using numeric collection counts. Unsafe values are refused. |
| Stream/message types, version numbers, range-anchor kinds, serialization flags, compression IDs, error/status codes | Safe `number`: numeric registries/bit fields, refused by `u53` before dispatch; narrower known registries add their own validation. IETF datagram type validates the full-width value before any bitwise operation. |
| Priorities, group order, booleans | A byte or a validated small enum; bigint property/parameter values are bounded before conversion. |
| Message/namespace/string/token/property/payload lengths, inflated sizes, list/parameter counts, path/hop prefix keep counts | Safe `number`: sizes and indices into JS buffers/collections. `u53` refuses unsafe values before allocation/read; existing message, frame, namespace and prefix bounds still apply. |
| Setup/parameter/property IDs and values, cluster hops, route costs, publisher epochs, announcement IDs, predecessor distances | `bigint`, or raw bytes decoded with full-width varints. Only explicitly bounded typed accessors narrow them. Unknown properties remain skippable without conversion. |

`integers.test.ts` exercises moved fields above 2^53 and independent hostile
encodings of numeric fields. `stream.test.ts` covers the shared safe-number
primitive for both varint formats, including fragmented input. Existing auth,
max-age, track-info and namespace suites cover their narrower bounds.
