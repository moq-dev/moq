import { expect, test } from "bun:test";
import type { Browser, Page } from "playwright";
import { open, readPlayerState, waitForWatch } from "./harness";
import { SAMPLE_EVENT } from "./src/contract";

// A page that logs samples on demand and refuses evaluation, since evaluating would grant
// user activation before the gesture checks.
function fakePage() {
	const listeners = new Map<string, (arg: unknown) => void>();
	const frame = {};
	const page = {
		on(event: string, listener: (arg: unknown) => void) {
			listeners.set(event, listener);
		},
		mainFrame: () => frame,
		evaluate() {
			throw new Error("evaluation would grant user activation");
		},
		goto() {},
	} as unknown as Page;
	const log = (sample: object) => listeners.get("console")?.({ text: () => SAMPLE_EVENT + JSON.stringify(sample) });
	const navigate = () => listeners.get("framenavigated")?.(frame);
	return { page, log, navigate, browser: { newPage: () => page } as unknown as Browser };
}

test("player observations never evaluate in the page", async () => {
	const { page, log, browser } = fakePage();
	await open(browser, "http://localhost");
	log({ seq: 1, userActivated: false, centerPlayVisible: true, controlLabel: "Play" });
	await waitForWatch(page);
	expect(await readPlayerState(page)).toMatchObject({
		seq: 1,
		userActivated: false,
		centerPlayVisible: true,
		controlLabel: "Play",
	});
});

test("a navigation discards the previous document's sample", async () => {
	const { page, log, navigate, browser } = fakePage();
	await open(browser, "http://localhost");
	log({ seq: 7 });
	navigate();
	await expect(readPlayerState(page)).rejects.toThrow("has not published a sample");
	log({ seq: 0 });
	expect((await readPlayerState(page)).seq).toBe(0);
});
