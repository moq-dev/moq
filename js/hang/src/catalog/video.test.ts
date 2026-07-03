import { expect, test } from "bun:test";
import { VideoConfigSchema, VideoSchema } from "./video.ts";

test("video config accepts canonical display aspect fields", () => {
	const parsed = VideoConfigSchema.parse({
		codec: "avc1.64001f",
		container: { kind: "legacy" },
		displayAspectWidth: 4,
		displayAspectHeight: 3,
	});

	expect(Number(parsed.displayAspectWidth)).toBe(4);
	expect(Number(parsed.displayAspectHeight)).toBe(3);
	expect("displayRatioWidth" in parsed).toBe(false);
	expect("displayRatioHeight" in parsed).toBe(false);
});

test("legacy video arrays derive display size from display aspect fields", () => {
	const parsed = VideoSchema.parse([
		{
			track: { name: "video" },
			config: {
				codec: "avc1.64001f",
				container: { kind: "legacy" },
				displayAspectWidth: 16,
				displayAspectHeight: 9,
			},
		},
	]);

	expect(
		parsed.display && {
			width: Number(parsed.display.width),
			height: Number(parsed.display.height),
		},
	).toEqual({ width: 16, height: 9 });
	expect(Number(parsed.renditions.video?.displayAspectWidth)).toBe(16);
	expect(Number(parsed.renditions.video?.displayAspectHeight)).toBe(9);
});
