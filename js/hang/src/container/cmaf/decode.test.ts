import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { decodeDataSegment, decodeInitSegment } from "./decode.ts";

// Shared with moq-mux, which decodes the same fragments, so both readers resolve a sample's size,
// duration, and keyframe bit the same way.
const FIXTURES = join(
	dirname(fileURLToPath(import.meta.url)),
	"../../../../../rs/moq-mux/src/container/fmp4/test_data/sample-defaults.json",
);

interface Fixture {
	name: string;
	init: string;
	fragment: string;
	samples: { timestamp: number; duration: number; keyframe: boolean; data: number[] }[];
}

const { cases }: { cases: Fixture[] } = JSON.parse(readFileSync(FIXTURES, "utf8"));

describe("CMAF sample defaults", () => {
	for (const fixture of cases) {
		test(fixture.name, () => {
			const init = decodeInitSegment(new Uint8Array(Buffer.from(fixture.init, "base64")));
			const samples = decodeDataSegment(new Uint8Array(Buffer.from(fixture.fragment, "base64")), init, undefined);
			expect(
				samples.map(({ timestamp, duration, keyframe, data }) => ({
					timestamp,
					duration,
					keyframe,
					data: Array.from(data),
				})),
			).toEqual(fixture.samples);
		});
	}
});
