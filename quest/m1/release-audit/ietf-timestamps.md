# [S] Preserve timestamp conversion rules in JS IETF objects

## Goal

The JS IETF object encoder preserves exact same-scale timestamps and refuses
unrepresentable conversions before emitting data its receiver rejects.

## Plan

P2 inherited correctness defect at main 1d43c096e0.
`js/net/src/ietf/object.ts::encodeObjectTime` multiplies and divides Numbers
directly, bypassing Timestamp.as and its same-scale shortcut and range check.
A valid NANO timestamp 9007199254740985 roundtrips as 9007199254740984 through
IETF. The normal conversion, Lite path, and native same-scale conversion
preserve it. This becomes reachable around 104 days on a nanosecond timeline.

Converting Timestamp.fromMillis(10_000_000_000) to the IETF nanosecond scale
also emits 10^16 ticks, exceeding the JS Timestamp range. Its own decoder
rejects the result, whereas Timestamp.as and Lite reject the conversion
before sending. These are local encoder/decoder reproductions using valid
API inputs, not end-to-end transport tests.

Use the shared timestamp conversion contract in IETF framing. Test exact
same-scale boundaries, supported conversions, and refusal before encoding
out-of-range results. Keep supported wire values unchanged; report any
observable error-behavior change. No new timestamp API is needed.

## Related

- [Binding timestamps](/quest/m1/binding-timestamps.md) - exact values through language bindings, separate from JS protocol encoding
