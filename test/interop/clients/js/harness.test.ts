import { expect, test } from "bun:test";
import type { Browser, Page } from "playwright";
import { open, readPlayerState, waitForWatch } from "./harness";
import { SAMPLE_EVENT } from "./src/contract";

test("Firefox player observations never evaluate in the page before its gesture", async () => {
	let consoleListener: (message: { text: () => string }) => void = () => {};
	const sample = { seq: 1, userActivated: false, centerPlayVisible: true, controlLabel: "Play" };
	const page = {
		on(event: string, listener: typeof consoleListener) {
			if (event === "console") consoleListener = listener;
		},
		context: () => ({ browser: () => ({ browserType: () => ({ name: () => "firefox" }) }) }),
		evaluate() {
			throw new Error("evaluation would grant user activation");
		},
		goto() {
			consoleListener({ text: () => SAMPLE_EVENT + JSON.stringify(sample) });
		},
	} as unknown as Page;
	const browser = { newPage: () => page } as unknown as Browser;
	await open(browser, "http://localhost");
	await waitForWatch(page);
	const observed = await readPlayerState(page);
	expect(observed.seq).toBe(1);
	expect(observed.userActivated).toBe(false);
	expect(observed.centerPlayVisible).toBe(true);
	expect(observed.controlLabel).toBe("Play");
});
