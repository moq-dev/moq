import { expect, test } from "bun:test";
import { decodeDataSegment } from "./decode.ts";
import { encodeDataSegment } from "./encode.ts";

for (const kind of ["audio", "video"] as const) {
	for (const keyframe of [false, true]) {
		test(`CMAF sample sync flag: kind=${kind}, keyframe=${keyframe}`, () => {
			const options = {
				data: new Uint8Array([0x01]),
				timestamp: 960,
				duration: 960,
				sequence: 2,
				kind,
				keyframe,
			};
			const segment = encodeDataSegment(options);
			// Read as video so the decoder exposes the encoded sync flag.
			const [sample] = decodeDataSegment(segment, {
				timescale: 48_000,
				trackId: 1,
				kind: "video",
				defaultSampleDuration: 0,
				defaultSampleSize: 0,
				defaultSampleFlags: 0,
			});
			expect(sample.keyframe).toBe(kind === "audio" || keyframe);
		});
	}
}
