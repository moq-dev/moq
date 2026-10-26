import { expect, test } from "bun:test";
import type MoqWatch from "@moq/watch/element";
import { createServer } from "vite";
import { worklet } from "../../../../js/common/vite-plugin-worklet";
import { launch, pause } from "./harness";

test("pause activates its button when a canvas intercepts pointer input", async () => {
	const browser = await launch();
	try {
		const page = await browser.newPage();
		await page.setContent(`
			<moq-watch-ui></moq-watch-ui>
			<script>
				const root = document.querySelector("moq-watch-ui").attachShadow({ mode: "open" });
				root.innerHTML = \`<button class="control" aria-label="Pause">Pause</button>
					<canvas style="position: fixed; inset: 0; width: 100%; height: 100%"></canvas>\`;
				root.querySelector("button").addEventListener("click", () => {
					document.body.dataset.paused = "true";
				});
			</script>
		`);
		await pause(page);
		expect(await page.locator("body").getAttribute("data-paused")).toBe("true");
	} finally {
		await browser.close();
	}
}, 30_000);

for (const stall of ["audio", "video"]) {
	test(`the paused play button stays clickable during ${stall} stalling`, async () => {
		const server = await createServer({
			configFile: false,
			root: `${import.meta.dir}/buffering`,
			server: { host: "127.0.0.1", port: 0 },
		});
		await server.listen();
		const browser = await launch();
		try {
			const page = await browser.newPage();
			await page.goto(`${server.resolvedUrls?.local[0]}?${stall === "video" ? "video" : ""}`);
			const spinner = page.locator(".buffering");
			await spinner.waitFor({ state: "visible" });
			await page.getByRole("button", { name: "Pause", exact: true }).click();
			await page.locator('body[data-paused="true"]').waitFor();
			await spinner.waitFor({ state: "hidden" });

			// Click normally: a forced click would miss the overlay intercepting pointer input.
			await page.locator(".center-play").click({ timeout: 5000 });
			await page.locator('body[data-paused="false"]').waitFor();
			await spinner.waitFor({ state: "visible" });
		} finally {
			await browser.close();
			await server.close();
		}
	}, 30_000);
}

test("watch mute and volume are independent in either attribute or property order", async () => {
	const server = await createServer({
		configFile: false,
		root: `${import.meta.dir}/watch-ui`,
		plugins: [worklet()],
		server: { host: "127.0.0.1", port: 0 },
	});
	await server.listen();
	const browser = await launch();
	try {
		const page = await browser.newPage();
		await page.goto(server.resolvedUrls?.local[0] ?? "");
		await page.waitForFunction(() => !!customElements.get("moq-watch"));
		const results = await page.evaluate(async () => {
			const settle = async () => {
				for (let i = 0; i < 5; i++) await new Promise<void>((resolve) => queueMicrotask(resolve));
			};
			const results = [];
			for (const mode of ["attribute", "property", "live-property"] as const)
				for (const volumeFirst of [true, false]) {
					let watch: MoqWatch;
					if (mode === "attribute") {
						document.body.insertAdjacentHTML(
							"beforeend",
							`<moq-watch ${volumeFirst ? 'volume="0.8" muted' : 'muted volume="0.8"'}></moq-watch>`,
						);
						watch = document.body.lastElementChild as MoqWatch;
					} else {
						watch = document.createElement("moq-watch") as MoqWatch;
						if (mode === "live-property") {
							document.body.append(watch);
							await settle();
						}
						if (volumeFirst) {
							watch.volume = 0.8;
							watch.muted = true;
						} else {
							watch.muted = true;
							watch.volume = 0.8;
						}
						if (!watch.isConnected) document.body.append(watch);
					}
					await settle();
					const muted = {
						volume: watch.volume,
						muted: watch.muted,
						attribute: watch.getAttribute("volume"),
						emitterVolume: watch.emitter.in.volume.peek(),
						emitterMuted: watch.emitter.in.muted.peek(),
					};
					watch.muted = false;
					await settle();
					const unmuted = { volume: watch.volume, muted: watch.muted };
					watch.volume = 0;
					await settle();
					const zero = { volume: watch.volume, muted: watch.muted };
					watch.muted = true;
					await settle();
					watch.muted = false;
					await settle();
					results.push({ mode, volumeFirst, muted, unmuted, zero, zeroAfterUnmute: watch.volume });
					watch.remove();
					watch.signals.close();
				}
			return results;
		});
		for (const result of results) {
			expect(result.muted).toEqual({
				volume: 0.8,
				muted: true,
				attribute: "0.8",
				emitterVolume: 0.8,
				emitterMuted: true,
			});
			expect(result.unmuted).toEqual({ volume: 0.8, muted: false });
			expect(result.zero).toEqual({ volume: 0, muted: false });
			expect(result.zeroAfterUnmute).toBe(0);
		}
	} finally {
		await browser.close();
		await server.close();
	}
}, 30_000);

test("the real gain node uses mute without changing the selected volume", async () => {
	const server = await createServer({
		configFile: false,
		root: `${import.meta.dir}/watch-ui`,
		plugins: [worklet()],
		server: { host: "127.0.0.1", port: 0 },
	});
	await server.listen();
	const browser = await launch();
	try {
		const page = await browser.newPage();
		await page.goto(server.resolvedUrls?.local[0] ?? "");
		await page.waitForFunction(() => !!customElements.get("moq-watch"));
		const results = await page.evaluate(async () => {
			// Vite serves the fixture as a browser module, including the shipped emitter.
			const { gain } = await import(`${location.origin}/main.ts`);
			const watch = document.createElement("moq-watch") as MoqWatch;
			document.body.append(watch);
			try {
				const results = [];
				for (const [volume, muted] of [
					[0.8, true],
					[0.8, false],
					[0, false],
				] as const) {
					watch.volume = volume;
					watch.muted = muted;
					for (let i = 0; i < 5; i++) await new Promise<void>((resolve) => queueMicrotask(resolve));
					results.push(await gain(watch));
				}
				return results;
			} finally {
				watch.remove();
				watch.signals.close();
			}
		});
		expect(results[0]).toEqual({ level: 0, sample: 0, enabled: false });
		expect(results[1].level).toBeCloseTo(0.8);
		expect(results[1].sample).toBeCloseTo(0.8);
		expect(results[1].enabled).toBe(true);
		expect(results[2]).toEqual({ level: 0, sample: 0, enabled: true });
	} finally {
		await browser.close();
		await server.close();
	}
}, 30_000);

test("watch volume controls retain the level through mute and keep zero independent", async () => {
	const server = await createServer({
		configFile: false,
		root: `${import.meta.dir}/watch-ui`,
		plugins: [worklet()],
		server: { host: "127.0.0.1", port: 0 },
	});
	await server.listen();
	const browser = await launch();
	try {
		const page = await browser.newPage();
		await page.goto(server.resolvedUrls?.local[0] ?? "");
		await page.waitForFunction(() => !!customElements.get("moq-watch-ui"));
		await page.evaluate(() =>
			document.body.insertAdjacentHTML(
				"beforeend",
				'<moq-watch-ui><moq-watch volume="0.8" muted><canvas width="640" height="360"></canvas></moq-watch></moq-watch-ui>',
			),
		);
		const state = () =>
			page.evaluate(() => {
				const watch = document.querySelector<MoqWatch>("moq-watch");
				if (!watch) throw new Error("missing watch element");
				return { volume: watch.volume, muted: watch.muted };
			});
		const slider = page.getByRole("slider", { name: "Volume", exact: true });
		await page.getByRole("button", { name: "Unmute", exact: true }).click();
		expect(await state()).toEqual({ volume: 0.8, muted: false });
		expect(await slider.inputValue()).toBe("80");
		await page.getByRole("button", { name: "Mute", exact: true }).click();
		expect(await state()).toEqual({ volume: 0.8, muted: true });
		await slider.evaluate((slider) => {
			(slider as HTMLInputElement).value = "30";
			slider.dispatchEvent(new Event("input", { bubbles: true }));
		});
		expect(await state()).toEqual({ volume: 0.3, muted: false });
		await slider.evaluate((slider) => {
			(slider as HTMLInputElement).value = "0";
			slider.dispatchEvent(new Event("input", { bubbles: true }));
		});
		expect(await state()).toEqual({ volume: 0, muted: false });
		await page.getByRole("button", { name: "Mute", exact: true }).click();
		await page.getByRole("button", { name: "Unmute", exact: true }).click();
		expect(await state()).toEqual({ volume: 0, muted: false });
	} finally {
		await browser.close();
		await server.close();
	}
}, 30_000);
