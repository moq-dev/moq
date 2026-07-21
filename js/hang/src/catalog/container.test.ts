import { expect, test } from "bun:test";
import { ContainerSchema, containerSupported } from "./container.ts";
import { RootSchema } from "./root.ts";

test("known containers round-trip", () => {
	for (const container of [{ kind: "legacy" }, { kind: "cmaf", init: "AAEC" }, { kind: "loc" }]) {
		const parsed = ContainerSchema.parse(container);
		expect(parsed).toEqual(container);
		expect(containerSupported(parsed)).toBe(true);
	}
});

test("unknown container is preserved instead of throwing", () => {
	const container = { kind: "future", extra: { nested: [1, 2] }, flag: true };
	const parsed = ContainerSchema.parse(container);
	expect(parsed).toEqual(container);
	expect(containerSupported(parsed)).toBe(false);
});

test("catalog with an unknown container keeps its other renditions", () => {
	const catalog = {
		video: {
			renditions: {
				future: {
					codec: "avc1.64001f",
					container: { kind: "future", magic: 7 },
				},
				legacy: {
					codec: "avc1.64001f",
					codedWidth: 1280,
					codedHeight: 720,
					container: { kind: "legacy" },
				},
			},
		},
	};

	const parsed = RootSchema.parse(catalog);
	if (!parsed.video || !("renditions" in parsed.video)) throw new Error("missing video section");

	const known = parsed.video.renditions.legacy;
	expect(known?.container.kind).toBe("legacy");
	expect(Number(known?.codedWidth)).toBe(1280);

	// The unknown rendition survives a republish intact.
	expect(parsed.video.renditions.future?.container).toEqual({ kind: "future", magic: 7 });
});
