/** Demand gaps keep the device open without pumping frames, on both window capture paths. */
import { expect, test } from "bun:test";
import { createServer } from "vite";
import { launch } from "./harness";

for (const path of ["native", "element"]) {
	test(`unread ${path} video capture idles and resumes`, async () => {
		const server = await createServer({ server: { host: "127.0.0.1", port: 0 } });
		await server.listen();
		const browser = await launch(["--use-fake-device-for-media-stream", "--use-fake-ui-for-media-stream"]);
		try {
			const page = await browser.newPage();
			await page.goto(`${server.resolvedUrls?.local[0]}capture-idle.html${path === "element" ? "?element" : ""}`);
			await page.waitForFunction(() => window.captureIdle?.state().display);
			const cdp = await page.context().newCDPSession(page);
			const system = await browser.newBrowserCDPSession();
			await cdp.send("Performance.enable", { timeDomain: "threadTicks" });
			const cpu = async () => {
				const { metrics } = await cdp.send("Performance.getMetrics");
				const { processInfo } = await system.send("SystemInfo.getProcessInfo");
				return {
					main: metrics.find(({ name }) => name === "TaskDuration")?.value ?? 0,
					renderer: processInfo
						.filter(({ type }) => type === "renderer")
						.reduce((sum, { cpuTime }) => sum + cpuTime, 0),
				};
			};
			const seconds = Number(process.env.CAPTURE_IDLE_SECONDS ?? 0.25);
			const sample = async (phase: string) => {
				// Let the read already in flight settle before observing the idle window.
				await page.waitForTimeout(200);
				const before = await page.evaluate(() => window.captureIdle.state());
				const start = await cpu();
				await page.waitForTimeout(seconds * 1000);
				const end = await cpu();
				const after = await page.evaluate(() => window.captureIdle.state());
				const result = {
					path,
					phase,
					frames: after.processed - before.processed,
					main_ms_per_s: ((end.main - start.main) * 1000) / seconds,
					renderer_ms_per_s: ((end.renderer - start.renderer) * 1000) / seconds,
				};
				console.log(JSON.stringify(result));
				expect(after.failure).toBeUndefined();
				expect(after.device).toBe("live");
				if (phase !== "reading") expect(result.frames).toBe(0);
				return after;
			};
			await sample("initial-idle");
			await page.evaluate(() => window.captureIdle.attach(1));
			await page.waitForFunction(() => window.captureIdle.state().delivered >= 3);
			await page.evaluate(() => window.captureIdle.attach(2));
			await page.evaluate(() => window.captureIdle.detach(1));
			const reading = await sample("reading");
			expect(reading.delivered).toBeGreaterThan(3);
			await page.evaluate(() => window.captureIdle.detach(2));
			const idle = await sample("detached-idle");
			const start = performance.now();
			await page.evaluate(() => window.captureIdle.attach(3));
			await page.waitForFunction((previous) => window.captureIdle.state().delivered > previous, idle.delivered);
			console.log(JSON.stringify({ path, resume_ms: performance.now() - start }));
			await page.waitForFunction(
				(previous) => window.captureIdle.state().processed > previous + 2,
				idle.processed,
			);
			console.log(JSON.stringify({ path, live_resume_ms: performance.now() - start }));
			expect((await page.evaluate(() => window.captureIdle.state())).failure).toBeUndefined();
			await page.evaluate(() => window.captureIdle.close());
			expect((await page.evaluate(() => window.captureIdle.state())).device).toBe("ended");
		} finally {
			await browser.close();
			await server.close();
		}
	}, 30_000);
}
