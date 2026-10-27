import { expect, test } from "bun:test";
import type MoqPublish from "@moq/publish/element";
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

test("a selected camera stays announced while real capture is reopening", async () => {
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
		await page.getByRole("button", { name: /mute microphone/i }).waitFor();
		// Hold the browser capture promise after the element has stopped the previous track.
		const gate = await page.evaluateHandle(() => {
			const original = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
			const gate = {
				hold: false,
				pending: [] as (() => void)[],
				release() {
					this.hold = false;
					for (const resume of this.pending.splice(0)) resume();
				},
			};
			navigator.mediaDevices.getUserMedia = async (constraints) => {
				if (gate.hold) await new Promise<void>((resolve) => gate.pending.push(resolve));
				return original(constraints);
			};
			return gate;
		});
		await page.getByRole("button", { name: "Settings", exact: true }).click();
		await page
			.getByRole("tabpanel", { name: "Source" })
			.getByRole("button", { name: "Camera", exact: true })
			.click();
		await page.waitForFunction(() => {
			const publish = document.querySelector<MoqPublish>("moq-publish");
			return (
				publish?.broadcast.in.announce.peek() && publish.video.settled.peek() && publish.audio.settled.peek()
			);
		});
		const publication = await page.evaluateHandle(() => {
			const publish = document.querySelector<MoqPublish>("moq-publish");
			if (!publish) throw new Error("missing publish element");
			return { publish };
		});

		for (const kind of ["audio", "video"] as const) {
			await publication.evaluate(({ publish }, kind) => {
				if (kind === "audio") publish.muted = true;
				else publish.invisible = true;
			}, kind);
			await page.waitForFunction(({ publish, kind }) => !publish.sources[kind].peek()?.out.source.peek(), {
				publish: await publication.evaluateHandle(({ publish }) => publish),
				kind,
			});
			await gate.evaluate((gate) => {
				gate.hold = true;
			});
			await publication.evaluate(({ publish }, kind) => {
				if (kind === "audio") publish.muted = false;
				else publish.invisible = false;
			}, kind);
			await page.waitForFunction((gate) => gate.pending.length > 0, gate);
			expect(await publication.evaluate(({ publish }) => publish.broadcast.in.announce.peek())).toBe(true);
			await gate.evaluate((gate) => gate.release());
			await page.waitForFunction(({ publish, kind }) => !!publish.sources[kind].peek()?.out.source.peek(), {
				publish: await publication.evaluateHandle(({ publish }) => publish),
				kind,
			});
		}

		// A camera settings/device change also tears down capture before opening its replacement.
		await gate.evaluate((gate) => {
			gate.hold = true;
		});
		await publication.evaluate(({ publish }) => {
			const camera = publish.sources.video.peek();
			if (!camera || !("constraints" in camera)) throw new Error("missing camera");
			camera.constraints.set({ width: { ideal: 640 } });
		});
		await page.waitForFunction((gate) => gate.pending.length > 0, gate);
		expect(
			await publication.evaluate(({ publish }) => ({
				sourceMissing: publish.sources.video.peek()?.out.source.peek() === undefined,
				announcing: publish.broadcast.in.announce.peek(),
			})),
		).toEqual({ sourceMissing: true, announcing: true });
		await gate.evaluate((gate) => gate.release());
		await page.waitForFunction(({ publish }) => !!publish.sources.video.peek()?.out.source.peek(), publication);

		await publication.evaluate(({ publish }) => {
			publish.source = undefined;
		});
		await page.waitForFunction(({ publish }) => !publish.broadcast.in.announce.peek(), publication);
		await publication.evaluate(({ publish }) => {
			publish.remove();
		});
		await page.waitForFunction(({ publish }) => publish.broadcast.net.peek() === undefined, publication);
	} finally {
		await browser.close();
		await server.close();
	}
}, 30_000);
