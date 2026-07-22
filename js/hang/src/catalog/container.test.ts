import { expect, test } from "bun:test";
import { type Container, ContainerSchema, containerSupported } from "./container.ts";
import { RootSchema } from "./root.ts";

test("known containers round-trip", () => {
	const known: Container[] = [{ kind: "legacy" }, { kind: "cmaf", init: "AAEC" }, { kind: "loc" }];
	for (const container of known) {
		const parsed = ContainerSchema.parse(container);
		expect(parsed).toEqual(container);
		expect(containerSupported(parsed)).toBe(true);
	}
});

test("unknown container is preserved instead of throwing", () => {
	const container = { kind: "future", extra: { nested: [1, 2] }, flag: true };
	const parsed = ContainerSchema.parse(container);
	// Tagged with a literal `kind` so the union stays discriminated; `raw` is the original.
	expect(parsed).toEqual({ kind: "unknown", raw: container });
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

	// The unknown rendition keeps its original JSON verbatim under `raw`.
	expect(parsed.video.renditions.future?.container).toEqual({
		kind: "unknown",
		raw: { kind: "future", magic: 7 },
	});
});

test("a malformed known container errors instead of degrading to passthrough", () => {
	// `cmaf` without `init` fails its own schema. It must NOT fall through to the passthrough
	// arm, which would still report kind "cmaf" and hand decoders an undefined init segment.
	expect(() => ContainerSchema.parse({ kind: "cmaf" })).toThrow();

	// A genuinely unrecognized kind still parses, so one future rendition can't fail the catalog.
	expect(ContainerSchema.parse({ kind: "future", magic: 7 })).toEqual({
		kind: "unknown",
		raw: { kind: "future", magic: 7 },
	});
});
