/** Capture diagnostic uses the real browser and worklet, with each override isolated. */
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
		console.log(JSON.stringify({ platform: process.platform, browser: browser.version(), result }));
		expect(result).toHaveLength(5);
		expect(result.slice(0, 4)).toMatchObject([
			{ mode: "none", audible: true, format: { sampleRate: 48_000, channelCount: 2 } },
			{ mode: "rate", audible: true, format: { sampleRate: 48_000, channelCount: 2 } },
			{ mode: "channels", audible: true, format: { sampleRate: 48_000, channelCount: 1 } },
			{ mode: "both", audible: true, format: { sampleRate: 48_000, channelCount: 1 } },
		]);
	} finally {
		await browser.close();
		await server.close();
	}
}, 30_000);
