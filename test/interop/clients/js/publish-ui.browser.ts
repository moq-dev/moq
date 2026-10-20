import { expect, test } from "bun:test";
import type { Page } from "playwright";
import { createServer } from "vite";
import { worklet } from "../../../../js/common/vite-plugin-worklet";
import { launch } from "./harness";

/** What `<moq-publish>` was told to capture. */
function state(page: Page) {
	return page.evaluate(() => {
		const publish = document.querySelector("moq-publish") as unknown as {
			source?: string;
			muted: boolean;
			invisible: boolean;
		};
		return { source: publish.source, muted: publish.muted, invisible: publish.invisible };
	});
}

test("the publish controls start idle and drive source, mute, and visibility", async () => {
	const server = await createServer({
		configFile: false,
		root: `${import.meta.dir}/publish-ui`,
		plugins: [worklet()],
		server: { host: "127.0.0.1", port: 0 },
	});
	await server.listen();
	const browser = await launch(["--use-fake-device-for-media-stream", "--use-fake-ui-for-media-stream"]);
	try {
		const page = await browser.newPage();
		await page.goto(server.resolvedUrls?.local[0] ?? "");
		const mute = page.getByRole("button", { name: /mute microphone/i });
		await mute.waitFor();

		// Nothing captures until the viewer picks a source.
		expect(await state(page)).toEqual({ source: undefined, muted: true, invisible: true });
		expect(await mute.isDisabled()).toBe(true);

		await page.getByRole("button", { name: "Settings", exact: true }).click();
		const source = (name: string) =>
			page.getByRole("tabpanel", { name: "Source" }).getByRole("button", { name, exact: true });
		await source("Camera").click();
		expect(await state(page)).toEqual({ source: "camera", muted: false, invisible: false });

		await page.getByRole("button", { name: "Hide video", exact: true }).click();
		await page.getByRole("button", { name: "Mute microphone", exact: true }).click();
		expect(await state(page)).toEqual({ source: "camera", muted: true, invisible: true });

		await source("Off").click();
		expect(await state(page)).toEqual({ source: undefined, muted: true, invisible: true });
	} finally {
		await browser.close();
		await server.close();
	}
}, 30_000);
