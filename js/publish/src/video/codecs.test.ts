import { describe, expect, test } from "bun:test";
import { hardwareCodecs, SOFTWARE_CODECS } from "./codecs";

const family = (codec: string) => codec.split(".")[0];

describe("hardwareCodecs", () => {
	test("offers Safari only the codecs VideoToolbox actually accelerates", () => {
		const families = [...new Set(hardwareCodecs(true).map(family))];
		expect(families).toEqual(["avc1", "hev1"]);
	});

	test("prefers H.264 first on Safari, since that is the one every watcher can decode", () => {
		expect(hardwareCodecs(true)[0]).toBe("avc1.640028");
	});

	test("keeps the VP9-first order everywhere else", () => {
		const codecs = hardwareCodecs(false);
		expect(codecs[0]).toBe("vp09.00.10.08");
		expect([...new Set(codecs.map(family))]).toEqual(["vp09", "avc1", "av01", "hev1", "vp8"]);
	});
});

describe("SOFTWARE_CODECS", () => {
	test("prefers the cheap codecs and leaves AV1 for last", () => {
		expect(SOFTWARE_CODECS[0]).toBe("avc1.640028");
		expect(SOFTWARE_CODECS.at(-1)).toBe("av01");
	});
});
