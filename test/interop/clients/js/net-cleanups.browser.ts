import { expect, test } from "bun:test";
import { createServer } from "vite";
import { launch } from "./harness";

test("public net consumers and lite probes work in native browser streams", async () => {
	const server = await createServer({
		configFile: false,
		root: `${import.meta.dir}/net-cleanups`,
		server: { host: "127.0.0.1", port: 0 },
	});
	await server.listen();
	const browser = await launch();
	try {
		const page = await browser.newPage();
		const errors: string[] = [];
		page.on("pageerror", (error) => errors.push(error.message));
		await page.goto(server.resolvedUrls?.local[0] ?? "");
		const result = await page.evaluate(async () => (await import(`${location.origin}/main.ts`)).run());
		expect(result.cursors).toEqual(["first", "first", "second"]);
		expect(result.malformed).toBe(true);
		expect(result.value).toBe(false);
		expect(result.probes).toEqual([{ rate: 8_000_000 }, { rate: 8_000_000, rtt: 12 }]);
		expect(result.rows).toHaveLength(18);
		// Timings are evidence, not a noise-sensitive acceptance threshold. The shared sweep
		// itself checks complete delivery, shared payload identity and reader-independent bytes.
		console.log("browser compressed group fanout", JSON.stringify(result.rows));
		expect(errors).toEqual([]);
	} finally {
		await browser.close();
		await server.close();
	}
}, 30_000);
