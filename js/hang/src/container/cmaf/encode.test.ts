import { expect, test } from "bun:test";
import { readIsoBoxes, readTrun, type TrackRunBox } from "@svta/cml-iso-bmff";
import { encodeDataSegment } from "./encode.ts";

for (const kind of ["audio", "video"] as const) {
	for (const keyframe of [false, true]) {
		test(`${kind} sample flags with keyframe=${keyframe}`, () => {
			const segment = encodeDataSegment({
				kind,
				keyframe,
				data: new Uint8Array([1]),
				timestamp: 0,
				duration: 1000,
				sequence: 1,
			});
			const boxes = Array.from(readIsoBoxes(segment, { readers: { trun: readTrun } }));
			const moof = boxes.find((box) => box.type === "moof");
			if (!moof || !("boxes" in moof)) throw new Error("Missing moof");
			const traf = moof.boxes.find((box) => box.type === "traf");
			if (!traf || !("boxes" in traf)) throw new Error("Missing traf");
			const trun = traf.boxes.find((box) => box.type === "trun") as TrackRunBox;
			expect(trun.samples[0].sampleFlags).toBe(kind === "audio" || keyframe ? 0x02000000 : 0x01010000);
		});
	}
}
