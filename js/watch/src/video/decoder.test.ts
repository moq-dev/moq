import { describe, expect, it } from "bun:test";
import { configSuperseded } from "./decoder";

const base = { codec: "avc1.640028", container: { kind: "legacy" as const }, description: undefined };

describe("configSuperseded", () => {
	it("is false for the same config", () => {
		expect(configSuperseded(base, base)).toBe(false);
	});

	it("is false for a deep-equal but different object (the Signal.set identity trap)", () => {
		expect(configSuperseded({ ...base, container: { kind: "legacy" } }, base)).toBe(false);
	});

	it("is true on a codec change", () => {
		expect(configSuperseded({ ...base, codec: "vp8" }, base)).toBe(true);
	});

	it("is true on a container-kind change", () => {
		expect(configSuperseded({ ...base, container: { kind: "loc" } }, base)).toBe(true);
	});

	it("is true on a description change", () => {
		expect(configSuperseded({ ...base, description: "0164" }, base)).toBe(true);
	});

	it("is false when there is no current config (nothing to supersede yet)", () => {
		expect(configSuperseded(undefined, base)).toBe(false);
	});
});
