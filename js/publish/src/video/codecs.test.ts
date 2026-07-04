import { expect, test } from "bun:test";
import { hardwareCodecOrder, softwareCodecOrder } from "./codecs.ts";

test("Safari hardware order offers only the codecs it actually hardware-encodes", () => {
	const order = hardwareCodecOrder(true);

	// H.264 first (the requested default), HEVC after; both are VideoToolbox hardware.
	expect(order[0]).toBe("avc1.640028");
	expect(order).toContain("hev1.1.6.L93.B0");
	// avc1 must beat hev1: H.264 has universal decode support (Firefox can't decode HEVC).
	expect(order.indexOf("avc1.640028")).toBeLessThan(order.indexOf("hev1.1.6.L93.B0"));

	// Safari has no hardware VP9/VP8/AV1 encoder, so they must not appear in the hardware pass
	// (that is the bug: Safari reports software VP9 as hardware-supported).
	expect(order.some((c) => c.startsWith("vp09"))).toBe(false);
	expect(order.some((c) => c.startsWith("vp8"))).toBe(false);
	expect(order.some((c) => c.startsWith("av01"))).toBe(false);
});

test("non-Safari hardware order keeps the decode-friendly VP9-first preference", () => {
	const order = hardwareCodecOrder(false);

	expect(order[0]).toBe("vp09.00.10.08");
	// VP9 stays ahead of H.264, unchanged from the original behavior (Chrome et al).
	expect(order.indexOf("vp09.00.10.08")).toBeLessThan(order.indexOf("avc1.640028"));
	// The full generic set is still offered.
	for (const family of ["vp09", "avc1", "av01", "hev1", "vp8"]) {
		expect(order.some((c) => c.startsWith(family))).toBe(true);
	}
});

test("software order prefers cheap-to-encode H.264 first and AV1 last", () => {
	const order = softwareCodecOrder();

	expect(order[0]).toBe("avc1.640028");
	expect(order[order.length - 1]).toBe("av01");
	// VP9 sits behind H.264 (more expensive to encode in software).
	expect(order.indexOf("vp09.00.10.08")).toBeGreaterThan(order.indexOf("avc1.640028"));
});
