import { expect, test } from "bun:test";
import { Root } from "./index.ts";

test("sd encoder defaults to a quarter-resolution scale", () => {
	const root = new Root({ sd: { enabled: true } });
	// Scales relative to the source rather than a fixed resolution.
	expect(root.sd.config.peek()?.maxScale).toBe(0.1875);
	expect(root.sd.config.peek()?.maxPixels).toBeUndefined();
	// hd stays uncapped; it tracks the source resolution.
	expect(root.hd.config.peek()).toBeUndefined();
	root.close();
});

test("an explicit sd config overrides the default", () => {
	// 1234 is arbitrary; it just needs to differ from the default.
	const root = new Root({ sd: { enabled: true, config: { maxPixels: 1234 } } });
	expect(root.sd.config.peek()?.maxPixels).toBe(1234);
	expect(root.sd.config.peek()?.maxScale).toBeUndefined();
	root.close();
});

test("an explicit empty sd config opts out of the default cap", () => {
	// Unlike omitting config entirely, `config: {}` takes ownership and skips the default cap.
	const root = new Root({ sd: { enabled: true, config: {} } });
	expect(root.sd.config.peek()?.maxPixels).toBeUndefined();
	root.close();
});
