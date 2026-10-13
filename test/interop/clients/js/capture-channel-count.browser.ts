/** Capture diagnostic uses the real browser and worklet, with each format override isolated. */
import { expect, test } from "bun:test";
import { createServer } from "vite";
import { launch } from "./harness";

test("capture format override matrix", async () => {
	const server = await createServer({ server: { host: "127.0.0.1", port: 0 } });
	await server.listen();
	const browser = await launch(["--autoplay-policy=no-user-gesture-required"]);
	try {
		const page = await browser.newPage();
		await page.goto(`${server.resolvedUrls?.local[0]}capture-channel-count.html`);
		await page.waitForFunction(() => typeof window.captureMatrix === "function");
		const result = await page.evaluate(() => window.captureMatrix());
		expect(result).toEqual([
			{ mode: "none", channelsAudible: [true, true], gaps: 0, format: { sampleRate: 48_000, channelCount: 2 } },
			{ mode: "rate", channelsAudible: [true, true], gaps: 0, format: { sampleRate: 48_000, channelCount: 2 } },
			{ mode: "channels", channelsAudible: [true], gaps: 0, format: { sampleRate: 48_000, channelCount: 1 } },
			{ mode: "both", channelsAudible: [true], gaps: 0, format: { sampleRate: 48_000, channelCount: 1 } },
		]);
	} finally {
		await browser.close();
		await server.close();
	}
}, 30_000);
