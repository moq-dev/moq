import { expect, test } from "bun:test";
import type MoqPublish from "@moq/publish/element";
import type { JSHandle, Page } from "playwright";
import { createServer } from "vite";
import { worklet } from "../../../../js/common/vite-plugin-worklet";
import { launch } from "./harness";

/** The microphone and camera tracks a `<moq-publish>` holds open, in that order. */
function captured(handle: JSHandle<{ publish: MoqPublish }>) {
	return handle.evaluateHandle(({ publish }) => {
		const audio = publish.sources.audio.peek()?.out.source.peek()?.audio;
		const video = publish.sources.video.peek()?.out.source.peek()?.video;
		const track = (source: unknown) =>
			source instanceof MediaStreamTrack ? source : (source as { track?: MediaStreamTrack } | undefined)?.track;
		return [track(audio), track(video)];
	});
}

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
		// Wait for real capture so the clicks below cannot race the live layout change.
		await page.waitForFunction(() => {
			const publish = document.querySelector<MoqPublish>("moq-publish");
			return !!publish?.sources.video.peek()?.out.source.peek();
		});
		expect(await page.locator("moq-publish video, moq-publish canvas").count()).toBe(0);
		const player = await page.locator("moq-publish-ui .player").boundingBox();
		expect(player?.height).toBeGreaterThan(0);
		expect(await state(page)).toEqual({ source: "camera", muted: false, invisible: false });
		expect(await page.locator("moq-publish").getAttribute("source")).toBe("camera");

		await page.getByRole("button", { name: "Hide video", exact: true }).click();
		await page.getByRole("button", { name: "Mute microphone", exact: true }).click();
		expect(await state(page)).toEqual({ source: "camera", muted: true, invisible: true });
		expect(await page.locator("moq-publish").getAttribute("muted")).toBe("");
		await page.getByRole("button", { name: "Unmute microphone", exact: true }).click();
		await page.waitForFunction(() => {
			const publish = document.querySelector("moq-publish");
			return publish !== null && !publish.hasAttribute("muted");
		});
		await page.evaluate(() => {
			const publish = document.querySelector("moq-publish");
			if (!publish) throw new Error("missing publish element");
			publish.setAttribute("muted", "");
		});
		expect((await state(page)).muted).toBe(true);

		await source("Off").click();
		expect(await state(page)).toEqual({ source: undefined, muted: true, invisible: true });
	} finally {
		await browser.close();
		await server.close();
	}
}, 30_000);

test.each(["video", "canvas"] as const)(
	"publish controls follow a %s preview's rendered size",
	async (tag) => {
		const server = await createServer({
			configFile: false,
			root: `${import.meta.dir}/publish-ui`,
			plugins: [worklet()],
			server: { host: "127.0.0.1", port: 0 },
		});
		await server.listen();
		const browser = await launch(["--use-fake-device-for-media-stream", "--use-fake-ui-for-media-stream"]);
		try {
			const page = await browser.newPage({ viewport: { width: 960, height: 720 } });
			await page.goto(server.resolvedUrls?.local[0] ?? "");
			await page.getByRole("button", { name: "Settings", exact: true }).waitFor();
			await page.evaluate((tag) => {
				const publish = document.querySelector<MoqPublish>("moq-publish");
				if (!publish) throw new Error("missing publish element");
				publish.backend = "2d";
				publish.capture = "hold";
				const preview = document.createElement(tag);
				preview.style.width = "100%";
				preview.style.height = "auto";
				if (preview instanceof HTMLVideoElement) {
					preview.autoplay = true;
					preview.muted = true;
				}
				publish.append(preview);
			}, tag);
			const publication = await page.evaluateHandle(() => {
				const publish = document.querySelector<MoqPublish>("moq-publish");
				if (!publish) throw new Error("missing publish element");
				return { publish };
			});
			const player = page.locator("moq-publish-ui .player");
			const preview = page.locator(`moq-publish ${tag}`);
			const empty = (expected: boolean) =>
				page.waitForFunction((expected) => {
					const player = document.querySelector("moq-publish-ui")?.shadowRoot?.querySelector(".player");
					return !!player && player.classList.contains("player--empty") === expected;
				}, expected);
			const hidden = async () => {
				await page.waitForFunction((tag) => {
					const publish = document.querySelector("moq-publish");
					const preview = publish?.querySelector(tag);
					return preview && getComputedStyle(preview).display === "none";
				}, tag);
				await empty(true);
				expect(await preview.boundingBox()).toBeNull();
				expect((await page.locator("moq-publish").boundingBox())?.height).toBe(0);
				if (tag === "video")
					expect(await preview.evaluate((preview) => (preview as HTMLVideoElement).srcObject)).toBeNull();
			};
			// Idle preview elements must not contribute their default 300x150 size.
			await hidden();
			const idle = await player.boundingBox();
			expect(idle?.height).toBeCloseTo(((idle?.width ?? 0) * 9) / 16, 0);
			if (tag === "canvas")
				await preview.evaluate((preview) => {
					(preview as HTMLElement).style.display = "none";
				});
			await page.getByRole("button", { name: "Settings", exact: true }).click();
			await page
				.getByRole("tabpanel", { name: "Source" })
				.getByRole("button", { name: "Camera", exact: true })
				.click();
			// Both preview types must recover and size the box from real captured frames.
			const shown = async () => {
				await page.waitForFunction((tag) => {
					const publish = document.querySelector<MoqPublish>("moq-publish");
					const preview = publish?.querySelector(tag);
					const player = publish?.parentElement?.shadowRoot?.querySelector(".player");
					const display = publish?.video.in.capture.peek()?.out.display.peek();
					return (
						preview &&
						player &&
						preview.getBoundingClientRect().height > 0 &&
						!player.classList.contains("player--empty") &&
						(preview instanceof HTMLVideoElement
							? preview.videoWidth > 0
							: display && preview.width === display.width && preview.height === display.height) &&
						Math.abs(player.getBoundingClientRect().height - preview.getBoundingClientRect().height) < 0.5
					);
				}, tag);
				// These cases hide a settled preview. A layout read can precede ResizeObserver delivery,
				// so let the UI observe the displayed size before hiding it.
				await publication.evaluate(
					({ publish }) =>
						new Promise<void>((resolve) => {
							const resize = new ResizeObserver(() => {
								resize.disconnect();
								resolve();
							});
							resize.observe(publish);
						}),
				);
			};
			if (tag === "canvas") {
				await page.waitForFunction(
					({ publish }) => !!publish.video.in.capture.peek()?.out.display.peek(),
					publication,
				);
				await hidden();
				expect(await preview.evaluate((preview) => (preview as HTMLElement).style.display)).toBe("none");
				await preview.evaluate((preview) => {
					(preview as HTMLElement).style.display = "";
				});
			}
			await shown();
			await empty(false);
			const visible = await player.boundingBox();
			expect(visible?.height).toBeGreaterThan(0);
			await page.waitForFunction(
				({ publish }) => publish.video.settled.peek() && publish.audio.settled.peek(),
				publication,
			);
			const opened = await captured(publication);
			// Hiding a held camera or just its preview must not reopen either capture device.
			for (const control of ["invisible", "preview"] as const) {
				await publication.evaluate(({ publish }, control) => {
					if (control === "invisible") publish.invisible = true;
					else publish.preview = "none";
				}, control);
				await hidden();
				expect((await player.boundingBox())?.height).toBeCloseTo(visible?.height ?? 0, 0);
				expect(await opened.evaluate((tracks) => tracks.map((track) => track?.readyState))).toEqual([
					"live",
					"live",
				]);
				await publication.evaluate(({ publish }, control) => {
					if (control === "invisible") publish.invisible = false;
					else publish.preview = "source";
				}, control);
				await shown();
				expect((await preview.boundingBox())?.height).toBeCloseTo(visible?.height ?? 0, 0);
				const resumed = await captured(publication);
				expect(
					await page.evaluate(
						([opened, resumed]) => opened.every((track, i) => track && track === resumed[i]),
						[opened, resumed] as const,
					),
				).toBe(true);
			}
			await publication.evaluate(({ publish }) => {
				publish.source = undefined;
			});
			await hidden();
			await page.waitForFunction((tracks) => tracks.every((track) => track?.readyState === "ended"), opened);
			await publication.evaluate(({ publish }) => {
				publish.source = "camera";
			});
			await empty(false);
			await page.waitForFunction(
				({ publish }) => publish.video.settled.peek() && publish.audio.settled.peek(),
				publication,
			);
			await shown();
			const reopened = await captured(publication);
			expect(await reopened.evaluate((tracks) => tracks.map((track) => track?.readyState))).toEqual([
				"live",
				"live",
			]);
			await publication.evaluate(({ publish }) => publish.remove());
			await page.waitForFunction((tracks) => tracks.every((track) => track?.readyState === "ended"), reopened);
			expect(
				await publication.evaluate(
					({ publish }, tag) => publish.querySelector<HTMLElement>(tag)?.style.display,
					tag,
				),
			).toBe(tag === "canvas" ? "" : "none");
			if (tag === "video")
				expect(
					await publication.evaluate(({ publish }) => publish.querySelector("video")?.srcObject),
				).toBeNull();
			await publication.evaluate(({ publish }) => {
				const ui = document.querySelector("moq-publish-ui");
				if (!ui) throw new Error("missing publish controls");
				ui.append(publish);
			});
			await shown();
			await preview.evaluate((preview) => {
				(preview as HTMLElement).style.display = "none";
			});
			await empty(true);
			expect((await player.boundingBox())?.height).toBeCloseTo(visible?.height ?? 0, 0);
			if (tag === "canvas") {
				// The page owns display even across control changes and while the component hides it.
				for (const control of ["invisible", "preview", "source"] as const) {
					await publication.evaluate(({ publish }, control) => {
						if (control === "invisible") publish.invisible = true;
						else if (control === "preview") publish.preview = "none";
						else publish.source = undefined;
					}, control);
					await hidden();
					await publication.evaluate(({ publish }, control) => {
						if (control === "invisible") publish.invisible = false;
						else if (control === "preview") publish.preview = "source";
						else publish.source = "camera";
					}, control);
					await page.waitForFunction(
						({ publish }) =>
							publish.video.settled.peek() && !!publish.video.in.capture.peek()?.out.display.peek(),
						publication,
					);
					await hidden();
					expect(await preview.evaluate((preview) => (preview as HTMLElement).style.display)).toBe("none");
				}
				await publication.evaluate(({ publish }) => {
					publish.preview = "none";
				});
				await hidden();
				await preview.evaluate((preview) => {
					(preview as HTMLElement).style.display = "inline-block";
				});
				await hidden();
				await publication.evaluate(({ publish }) => {
					publish.preview = "source";
				});
				await shown();
				expect(await preview.evaluate((preview) => getComputedStyle(preview).display)).toBe("inline-block");
			}
			// Removing a visible preview must also fall back to the reserved size.
			await preview.evaluate((preview) => {
				(preview as HTMLElement).style.display = "";
			});
			await shown();
			await preview.evaluate((preview) => preview.remove());
			await empty(true);
			expect(await page.locator("moq-publish video, moq-publish canvas").count()).toBe(0);
			expect((await player.boundingBox())?.height).toBeCloseTo(visible?.height ?? 0, 0);
			await player.hover();
			await page.getByRole("button", { name: "Hide video", exact: true }).click();
			await page.getByRole("button", { name: "Mute microphone", exact: true }).click();
			expect(await state(page)).toEqual({ source: "camera", muted: true, invisible: true });
		} finally {
			await browser.close();
			await server.close();
		}
	},
	30_000,
);

test("canvas previews wait for dimensions and paint fresh frames under a strict style policy", async () => {
	const server = await createServer({
		configFile: false,
		root: `${import.meta.dir}/publish-ui`,
		plugins: [
			worklet(),
			{
				name: "standalone-publish",
				transformIndexHtml: () =>
					'<!doctype html><html><body><moq-publish><canvas></canvas></moq-publish><script type="module" src="/main.ts"></script></body></html>',
			},
		],
		server: { host: "127.0.0.1", port: 0, headers: { "Content-Security-Policy": "style-src 'self'" } },
	});
	await server.listen();
	const browser = await launch(["--use-fake-device-for-media-stream", "--use-fake-ui-for-media-stream"]);
	try {
		const page = await browser.newPage();
		const errors: string[] = [];
		page.on("pageerror", (error) => errors.push(error.message));
		await page.goto(server.resolvedUrls?.local[0] ?? "");
		const fixture = await page.evaluateHandle(async () => {
			await customElements.whenDefined("moq-publish");
			const publish = document.querySelector<MoqPublish>("moq-publish");
			const preview = publish?.querySelector("canvas");
			if (!publish || !preview) throw new Error("missing preview");
			publish.backend = "2d";
			publish.capture = "hold";
			publish.muted = true;
			publish.style.display = "block";
			preview.style.width = "320px";
			preview.style.height = "auto";
			const fixture = {
				publish,
				preview,
				writer: undefined as WritableStreamDefaultWriter<VideoFrame> | undefined,
				violations: [] as string[],
			};
			document.addEventListener("securitypolicyviolation", (event) => {
				fixture.violations.push(event.effectiveDirective);
			});
			const original = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
			const { MediaStreamTrackGenerator } = globalThis as unknown as {
				MediaStreamTrackGenerator: new (init: {
					kind: "video";
				}) => MediaStreamTrack & {
					writable: WritableStream<VideoFrame>;
				};
			};
			navigator.mediaDevices.getUserMedia = async (constraints) => {
				if (!constraints?.video) return original(constraints);
				const track = new MediaStreamTrackGenerator({ kind: "video" });
				fixture.writer = track.writable.getWriter();
				return new MediaStream([track]);
			};
			return fixture;
		});
		const hidden = async () => {
			await page.waitForFunction(({ preview }) => getComputedStyle(preview).display === "none", fixture);
			expect(await fixture.evaluate(({ preview }) => preview.getBoundingClientRect().height)).toBe(0);
		};
		const frame = async (color: "red" | "blue", width = 64, height = 36) => {
			await fixture.evaluate(
				async (fixture, { color, width, height }) => {
					if (!fixture.writer) throw new Error("missing video writer");
					const canvas = new OffscreenCanvas(width, height);
					const context = canvas.getContext("2d");
					if (!context) throw new Error("missing frame context");
					context.fillStyle = color;
					context.fillRect(0, 0, width, height);
					// Live capture timestamps advance with real time, including time spent hidden.
					const frame = new VideoFrame(canvas, { timestamp: Math.round(performance.now() * 1000) });
					try {
						await fixture.writer.write(frame);
					} finally {
						frame.close();
					}
				},
				{ color, width, height },
			);
			await page.waitForFunction(
				({ preview, color, width, height }) => {
					const pixel = preview.getContext("2d")?.getImageData(0, 0, 1, 1).data;
					return (
						getComputedStyle(preview).display !== "none" &&
						preview.width === width &&
						preview.height === height &&
						pixel &&
						pixel[color === "red" ? 0 : 2] > 200 &&
						pixel[color === "red" ? 2 : 0] < 30
					);
				},
				{ preview: await fixture.getProperty("preview"), color, width, height },
			);
		};
		await hidden();
		await fixture.evaluate(({ publish }) => {
			publish.source = "camera";
		});
		await page.waitForFunction(({ publish }) => !!publish.video.in.capture.peek()?.in.source.peek(), fixture);
		// Acquisition alone must not expose the default 300x150 canvas before any frame arrives.
		await hidden();
		await frame("red");
		for (const control of ["invisible", "preview"] as const) {
			await fixture.evaluate(({ publish }, control) => {
				if (control === "invisible") publish.invisible = true;
				else publish.preview = "none";
			}, control);
			await hidden();
			await fixture.evaluate(({ publish }, control) => {
				if (control === "invisible") publish.invisible = false;
				else publish.preview = "source";
			}, control);
			// A different picture proves recovery draws new content, not merely a retained bitmap.
			await frame("blue");
			await frame("red");
		}
		await fixture.evaluate(({ publish }) => {
			publish.source = undefined;
		});
		await hidden();
		await fixture.evaluate(({ publish }) => {
			publish.source = "camera";
		});
		await page.waitForFunction(({ publish }) => !!publish.video.in.capture.peek()?.in.source.peek(), fixture);
		await hidden();
		await frame("blue", 128, 96);
		// A move to a shadow tree must attach the hiding rule to the new styling root.
		await fixture.evaluate(({ publish }) => {
			const host = document.createElement("div");
			document.body.append(host);
			host.attachShadow({ mode: "open" }).append(publish);
			publish.preview = "none";
		});
		await hidden();
		// Constructed stylesheets belong to their document, including after adoption into an iframe.
		await fixture.evaluate(({ publish }) => {
			const frame = document.createElement("iframe");
			document.body.append(frame);
			if (!frame.contentDocument) throw new Error("missing frame document");
			frame.contentDocument.body.append(publish);
		});
		await hidden();
		expect(await fixture.evaluate(({ violations }) => violations)).toEqual([]);
		expect(errors).toEqual([]);
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
				calls: 0,
				pending: [] as (() => void)[],
				release() {
					this.hold = false;
					for (const resume of this.pending.splice(0)) resume();
				},
			};
			navigator.mediaDevices.getUserMedia = async (constraints) => {
				gate.calls++;
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

		// Muting and hiding only flip each rendition to `enabled: false`: the same tracks stay live and
		// unmuting reopens nothing.
		const opened = await captured(publication);
		await gate.evaluate((gate) => {
			gate.hold = true;
		});
		const calls = await gate.evaluate((gate) => gate.calls);
		await publication.evaluate(({ publish }) => {
			publish.muted = true;
			publish.invisible = true;
		});
		await page.waitForFunction(
			({ publish }) =>
				publish.audio.out.catalog.peek()?.enabled === false &&
				publish.video.out.catalog.peek()?.enabled === false,
			publication,
		);
		expect(await opened.evaluate((tracks) => tracks.map((track) => track?.readyState))).toEqual(["live", "live"]);
		await publication.evaluate(({ publish }) => {
			publish.muted = false;
			publish.invisible = false;
		});
		await page.waitForFunction(
			({ publish }) =>
				publish.audio.out.catalog.peek()?.enabled === undefined &&
				publish.video.out.catalog.peek()?.enabled === undefined &&
				!!publish.audio.out.catalog.peek() &&
				!!publish.video.out.catalog.peek(),
			publication,
		);
		const resumed = await captured(publication);
		expect(
			await page.evaluate(([opened, resumed]) => opened.every((track, i) => track && track === resumed[i]), [
				opened,
				resumed,
			] as const),
		).toBe(true);
		expect(await publication.evaluate(({ publish }) => publish.broadcast.in.announce.peek())).toBe(true);
		expect(await gate.evaluate((gate) => gate.calls)).toBe(calls);
		await gate.evaluate((gate) => gate.release());

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

test("a camera holds both devices until its source clears or the element detaches", async () => {
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
		const publication = await page.evaluateHandle(() => {
			const publish = document.querySelector<MoqPublish>("moq-publish");
			if (!publish) throw new Error("missing publish element");
			return { publish };
		});
		const open = () =>
			page.waitForFunction(
				({ publish }) =>
					!!publish.sources.audio.peek()?.out.source.peek() &&
					!!publish.sources.video.peek()?.out.source.peek(),
				publication,
			);
		const states = (tracks: JSHandle<(MediaStreamTrack | undefined)[]>) =>
			tracks.evaluate((tracks) => tracks.map((track) => track?.readyState));

		// Muted and hidden from the start still opens both, since only `source` gates the devices.
		await publication.evaluate(({ publish }) => {
			publish.muted = true;
			publish.invisible = true;
			publish.source = "camera";
		});
		await open();
		const first = await captured(publication);
		expect(await states(first)).toEqual(["live", "live"]);

		await publication.evaluate(({ publish }) => {
			publish.source = undefined;
		});
		await page.waitForFunction((tracks) => tracks.every((track) => track?.readyState === "ended"), first);

		await publication.evaluate(({ publish }) => {
			publish.source = "camera";
		});
		await open();
		const second = await captured(publication);
		expect(await states(second)).toEqual(["live", "live"]);

		await publication.evaluate(({ publish }) => publish.remove());
		await page.waitForFunction((tracks) => tracks.every((track) => track?.readyState === "ended"), second);

		await publication.evaluate(({ publish }) => document.body.append(publish));
		await open();
		expect(await states(await captured(publication))).toEqual(["live", "live"]);
	} finally {
		await browser.close();
		await server.close();
	}
}, 30_000);

test('capture="release" closes each device while it is muted or hidden', async () => {
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
		const publication = await page.evaluateHandle(() => {
			const publish = document.querySelector<MoqPublish>("moq-publish");
			if (!publish) throw new Error("missing publish element");
			publish.setAttribute("capture", "release");
			return { publish };
		});
		expect(await publication.evaluate(({ publish }) => publish.capture)).toBe("release");
		// Wait until the microphone and camera are each open or closed as given.
		const opened = (audio: boolean, video: boolean) =>
			page.waitForFunction(
				([{ publish }, audio, video]) =>
					!!publish.sources.audio.peek()?.out.source.peek() === audio &&
					!!publish.sources.video.peek()?.out.source.peek() === video,
				[publication, audio, video] as const,
			);

		// Muted and hidden from the start opens neither device.
		await publication.evaluate(({ publish }) => {
			publish.muted = true;
			publish.invisible = true;
			publish.source = "camera";
		});
		expect(
			await publication.evaluate(({ publish }) =>
				[publish.sources.audio.peek(), publish.sources.video.peek()].map((source) => source?.in.enabled.peek()),
			),
		).toEqual([false, false]);

		await publication.evaluate(({ publish }) => {
			publish.muted = false;
			publish.invisible = false;
		});
		await opened(true, true);
		const first = await captured(publication);

		// Muting ends only the microphone; unmuting opens a new one.
		await publication.evaluate(({ publish }) => {
			publish.muted = true;
		});
		await page.waitForFunction((tracks) => tracks[0]?.readyState === "ended", first);
		expect(await first.evaluate((tracks) => tracks[1]?.readyState)).toBe("live");
		await publication.evaluate(({ publish }) => {
			publish.muted = false;
		});
		await opened(true, true);
		const second = await captured(publication);
		expect(await second.evaluate((tracks) => tracks.map((track) => track?.readyState))).toEqual(["live", "live"]);

		// Hiding ends only the camera.
		await publication.evaluate(({ publish }) => {
			publish.invisible = true;
		});
		await page.waitForFunction((tracks) => tracks[1]?.readyState === "ended", second);
		expect(await second.evaluate((tracks) => tracks[0]?.readyState)).toBe("live");
	} finally {
		await browser.close();
		await server.close();
	}
}, 30_000);

test("watch property and UI state reflects without resetting a selected publish File", async () => {
	const server = await createServer({
		configFile: false,
		root: `${import.meta.dir}/publish-ui`,
		plugins: [worklet()],
		server: { host: "127.0.0.1", port: 0 },
	});
	await server.listen();
	const browser = await launch();
	try {
		const page = await browser.newPage();
		await page.goto(server.resolvedUrls?.local[0] ?? "");
		await page.evaluate(async () => {
			await customElements.whenDefined("moq-watch");
			const watch = document.createElement("moq-watch");
			document.body.append(watch);
			watch.connection.enabled.set(false);
			watch.captions = "english";
			watch.controls.text.target.set("french");
			watch.follow = false;
			watch.catalogFormat = "manual";
			const publish = document.querySelector("moq-publish");
			if (!publish) throw new Error("missing publish element");
			// Keep the selected File itself, rather than echoing "file" through the source setter.
			const file = new File([], "selected.webm", { type: "video/webm" });
			publish.source = file;
			Object.assign(window, { selectedFile: file });
		});
		await page.waitForFunction(
			() => document.querySelector("moq-watch")?.getAttribute("captions") === "french",
			undefined,
			{ timeout: 5000 },
		);
		expect(
			await page.evaluate(() => {
				const watch = document.querySelector("moq-watch");
				if (!watch) throw new Error("missing watch element");
				const publish = document.querySelector("moq-publish");
				if (!publish) throw new Error("missing publish element");
				return {
					enabled: watch.connection.enabled.peek(),
					follow: watch.getAttribute("follow"),
					catalog: watch.getAttribute("catalog-format"),
					source: publish.getAttribute("source"),
					sameFile: publish.source === (window as unknown as { selectedFile: File }).selectedFile,
				};
			}),
		).toEqual({ enabled: false, follow: "false", catalog: "manual", source: "file", sameFile: true });
	} finally {
		await browser.close();
		await server.close();
	}
}, 30_000);

test("invalid markup reports its error and suspends the element until corrected", async () => {
	const server = await createServer({
		configFile: false,
		root: `${import.meta.dir}/publish-ui`,
		plugins: [worklet()],
		server: { host: "127.0.0.1", port: 0 },
	});
	await server.listen();
	const browser = await launch();
	try {
		const page = await browser.newPage();
		await page.goto(server.resolvedUrls?.local[0] ?? "");
		const result = await page.evaluate(async () => {
			await customElements.whenDefined("moq-watch");
			const errors: string[] = [];
			window.addEventListener("error", (event) => {
				errors.push(event.message);
				event.preventDefault();
			});
			const watch = document.createElement("moq-watch");
			watch.setAttribute("catalog-format", "guess");
			document.body.append(watch);
			return { errors, enabled: watch.connection.enabled.peek(), raw: watch.getAttribute("catalog-format") };
		});
		expect(result.errors.some((error) => error.includes("catalog-format"))).toBe(true);
		expect(result.enabled).toBe(false);
		expect(result.raw).toBe("guess");

		const renamed = await page.evaluate(() => {
			const errors: string[] = [];
			window.addEventListener("error", (event) => {
				errors.push(event.message);
				event.preventDefault();
			});
			const watch = document.createElement("moq-watch");
			watch.setAttribute("announced", "false");
			document.body.append(watch);
			const refused =
				!watch.connection.enabled.peek() && errors.some((error) => error.includes("renamed to `follow`"));
			watch.removeAttribute("announced");
			const recovered = watch.connection.enabled.peek();
			watch.remove();
			watch.signals.close();
			return { refused, recovered };
		});
		expect(renamed).toEqual({ refused: true, recovered: true });
	} finally {
		await browser.close();
		await server.close();
	}
}, 30_000);

test("invalid attributes and properties refuse, retain bad markup, and recover", async () => {
	const server = await createServer({
		configFile: false,
		root: `${import.meta.dir}/publish-ui`,
		plugins: [worklet()],
		server: { host: "127.0.0.1", port: 0 },
	});
	await server.listen();
	const browser = await launch();
	try {
		const page = await browser.newPage();
		await page.goto(server.resolvedUrls?.local[0] ?? "");
		const results = await page.evaluate(async () => {
			await customElements.whenDefined("moq-watch");
			const errors: string[] = [];
			window.addEventListener("error", (event) => {
				errors.push(event.message);
				event.preventDefault();
			});
			const cases: readonly (readonly ["moq-watch" | "moq-publish", string, string, string, unknown])[] = [
				["moq-watch", "url", "http://[", "url", undefined],
				["moq-publish", "url", "http://[", "url", undefined],
				...["foo", "Infinity", "1oops", "0x10", "-0.1", ""].map(
					(value) => ["moq-watch", "volume", value, "volume", 0.25] as const,
				),
				["moq-watch", "backend", "gpu", "backend", "auto"],
				["moq-watch", "visible", "sometimes", "visible", "never"],
				["moq-watch", "delay", "30", "delay", "instant"],
				["moq-watch", "buffer", "30", "buffer", 0],
				["moq-watch", "catalog-format", "guess", "catalogFormat", "manual"],
				["moq-publish", "source", "monitor", "source", undefined],
				["moq-publish", "preview", "mse", "preview", "none"],
				["moq-publish", "backend", "gpu", "backend", "auto"],
				["moq-publish", "announce", "never", "announce", "source"],
			];
			return cases.map(([tag, attribute, invalid, property, valid]) => {
				const element = document.createElement(tag);
				const count = errors.length;
				element.setAttribute(attribute, invalid);
				document.body.append(element);
				const refused =
					!element.connection.enabled.peek() &&
					element.getAttribute(attribute) === invalid &&
					errors.length === count + 1 &&
					(errors.at(-1)?.includes(attribute) ?? false);
				let threw = false;
				try {
					Reflect.set(element, property, invalid);
				} catch {
					threw = true;
				}
				Reflect.set(element, property, valid);
				const recovered = element.connection.enabled.peek();
				element.remove();
				element.signals.close();
				return { attribute, invalid, refused, threw, recovered };
			});
		});
		for (const result of results)
			expect(result).toEqual({ ...result, refused: true, threw: true, recovered: true });
		const blocked = await page.evaluate(() => {
			const watch = document.createElement("moq-watch");
			watch.setAttribute("backend", "bad");
			watch.setAttribute("catalog-format", "bad");
			document.body.append(watch);
			watch.backend = "2d";
			const stillBlocked = !watch.connection.enabled.peek();
			watch.removeAttribute("catalog-format");
			const recovered = watch.connection.enabled.peek();
			watch.remove();
			watch.signals.close();
			return { stillBlocked, recovered };
		});
		expect(blocked).toEqual({ stillBlocked: true, recovered: true });
		const disabled = await page.evaluate(() =>
			(["moq-watch", "moq-publish"] as const).map((tag) => {
				const element = document.createElement(tag);
				document.body.append(element);
				// Recovery must not override a connection the page disabled first.
				element.connection.enabled.set(false);
				element.setAttribute("backend", "bad");
				element.removeAttribute("backend");
				const kept = element.connection.enabled.peek();
				element.connection.enabled.set(true);
				element.setAttribute("backend", "bad");
				element.removeAttribute("backend");
				const resumed = element.connection.enabled.peek();
				element.remove();
				element.signals.close();
				return { tag, kept, resumed };
			}),
		);
		expect(disabled).toEqual([
			{ tag: "moq-watch", kept: false, resumed: true },
			{ tag: "moq-publish", kept: false, resumed: true },
		]);
	} finally {
		await browser.close();
		await server.close();
	}
}, 30_000);

test("version lists keep their defaults and explicit false booleans round-trip", async () => {
	const server = await createServer({
		configFile: false,
		root: `${import.meta.dir}/publish-ui`,
		plugins: [worklet()],
		server: { host: "127.0.0.1", port: 0 },
	});
	await server.listen();
	const browser = await launch();
	try {
		const page = await browser.newPage();
		await page.goto(server.resolvedUrls?.local[0] ?? "");
		const result = await page.evaluate(async () => {
			await customElements.whenDefined("moq-watch");
			const watch = document.createElement("moq-watch");
			const versions = [];
			for (const value of ["moq-transport-18", " moq-lite-06 , moq-transport-22 ", "", " , ", null]) {
				if (value === null) watch.removeAttribute("versions");
				else watch.setAttribute("versions", value);
				versions.push(watch.versions);
			}
			watch.setAttribute("follow", "false");
			const follow = watch.follow;
			watch.signals.close();
			return { versions, follow };
		});
		expect(result).toEqual({
			versions: [["moq-transport-18"], ["moq-lite-06", "moq-transport-22"], undefined, undefined, undefined],
			follow: false,
		});
	} finally {
		await browser.close();
		await server.close();
	}
}, 30_000);

test("version lists refuse unsupported, duplicate, and empty lists and snapshot valid ones", async () => {
	const server = await createServer({
		configFile: false,
		root: `${import.meta.dir}/publish-ui`,
		plugins: [worklet()],
		server: { host: "127.0.0.1", port: 0 },
	});
	await server.listen();
	const browser = await launch();
	try {
		const page = await browser.newPage();
		await page.goto(server.resolvedUrls?.local[0] ?? "");
		const results = await page.evaluate(async () => {
			await customElements.whenDefined("moq-watch");
			const errors: string[] = [];
			window.addEventListener("error", (event) => {
				errors.push(event.message);
				event.preventDefault();
			});
			const results = [];
			for (const tag of ["moq-watch", "moq-publish"] as const) {
				const element = document.createElement(tag);
				const markup = ["moq-lite-99", "moq-lite-06,moq-lite-06"].map((invalid) => {
					const count = errors.length;
					element.setAttribute("versions", invalid);
					document.body.append(element);
					const refused =
						!element.connection.enabled.peek() &&
						element.getAttribute("versions") === invalid &&
						errors.length === count + 1;
					element.removeAttribute("versions");
					return refused && element.connection.enabled.peek();
				});
				const thrown = [[], ["moq-lite-99"], ["moq-lite-06", "moq-lite-06"]].map((invalid) => {
					try {
						element.versions = invalid;
						return false;
					} catch (error) {
						return error instanceof TypeError && element.versions === undefined;
					}
				});
				const list = ["moq-lite-06", "moq-transport-18"];
				element.versions = list;
				list.push("moq-lite-05");
				const snapshot = element.versions;
				// Reflection runs in an effect once the signal write settles.
				await new Promise((resolve) => setTimeout(resolve, 0));
				const reflected = element.getAttribute("versions");
				element.remove();
				element.signals.close();
				results.push({ tag, markup, thrown, snapshot, reflected });
			}
			return results;
		});
		for (const result of results)
			expect(result).toEqual({
				tag: result.tag,
				markup: [true, true],
				thrown: [true, true, true],
				snapshot: ["moq-lite-06", "moq-transport-18"],
				reflected: "moq-lite-06,moq-transport-18",
			});
	} finally {
		await browser.close();
		await server.close();
	}
}, 30_000);

test("the watch volume slider corrects a refused volume attribute", async () => {
	const server = await createServer({
		configFile: false,
		root: `${import.meta.dir}/publish-ui`,
		plugins: [worklet()],
		server: { host: "127.0.0.1", port: 0 },
	});
	await server.listen();
	const browser = await launch();
	try {
		const page = await browser.newPage();
		await page.goto(server.resolvedUrls?.local[0] ?? "");
		const refused = await page.evaluate(async () => {
			await customElements.whenDefined("moq-watch-ui");
			window.addEventListener("error", (event) => event.preventDefault());
			const ui = document.createElement("moq-watch-ui");
			const watch = document.createElement("moq-watch");
			watch.setAttribute("volume", "loud");
			ui.append(watch);
			document.body.append(ui);
			return watch.connection.enabled.peek();
		});
		expect(refused).toBe(false);
		const slider = await page.waitForFunction(() =>
			document.querySelector("moq-watch-ui")?.shadowRoot?.querySelector<HTMLInputElement>(".volume-track"),
		);
		await slider.evaluate((input) => {
			if (!input) throw new Error("missing volume slider");
			input.value = "80";
			input.dispatchEvent(new Event("input"));
		});
		await page.waitForFunction(
			() => document.querySelector("moq-watch")?.getAttribute("volume") === "0.8",
			undefined,
			{ timeout: 5000 },
		);
		expect(await page.evaluate(() => document.querySelector("moq-watch")?.connection.enabled.peek())).toBe(true);
	} finally {
		await browser.close();
		await server.close();
	}
}, 30_000);
