import { expect, test } from "bun:test";
import { Root } from "./index.ts";

test("sd encoder defaults to a 480p pixel cap", () => {
	const root = new Root({ sd: { enabled: true } });
	expect(root.sd.config.peek()?.maxPixels).toBe(854 * 480);
	// hd stays uncapped; it tracks the source resolution.
	expect(root.hd.config.peek()).toBeUndefined();
	root.close();
});

test("an explicit sd config overrides the default", () => {
	// 1234 is arbitrary; it just needs to differ from the 480p default.
	const root = new Root({ sd: { enabled: true, config: { maxPixels: 1234 } } });
	expect(root.sd.config.peek()?.maxPixels).toBe(1234);
	root.close();
});

test("an explicit empty sd config opts out of the default cap", () => {
	// Unlike omitting config entirely, `config: {}` takes ownership and skips the default cap.
	const root = new Root({ sd: { enabled: true, config: {} } });
	expect(root.sd.config.peek()?.maxPixels).toBeUndefined();
	root.close();
});
