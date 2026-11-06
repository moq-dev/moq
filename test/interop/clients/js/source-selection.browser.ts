import { expect, test } from "bun:test";
import { createServer } from "vite";
import { worklet } from "../../../../js/common/vite-plugin-worklet";
import { launch } from "./harness";

test("native decoder probes isolate malformed audio and conflicting video metadata", async () => {
	const server = await createServer({
		configFile: false,
		root: `${import.meta.dir}/source-selection`,
		plugins: [worklet()],
		server: { host: "127.0.0.1", port: 0 },
	});
	await server.listen();
	const browser = await launch();
	try {
		const page = await browser.newPage();
		const errors: string[] = [];
		page.on("pageerror", (error) => errors.push(error.message));
		await page.goto(server.resolvedUrls?.local[0] ?? "");
		await page
			.locator(
				'body[data-audio="good"][data-audio-available="good"][data-video="picture"][data-video-available="2"]',
			)
			.waitFor();
		await page.getByRole("button", { name: "Only malformed audio", exact: true }).click();
		await page.locator('body[data-audio=""][data-audio-available=""]').waitFor();
		await page.getByRole("button", { name: "Recover audio", exact: true }).click();
		await page
			.locator('body[data-audio="recovered"][data-audio-available="recovered"][data-video="picture"]')
			.waitFor();
		expect(errors).toEqual([]);
	} finally {
		await browser.close();
		await server.close();
	}
}, 30000);
