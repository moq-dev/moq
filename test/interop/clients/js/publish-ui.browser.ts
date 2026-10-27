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
			watch.announced = false;
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
					announced: watch.getAttribute("announced"),
					catalog: watch.getAttribute("catalog-format"),
					source: publish.getAttribute("source"),
					sameFile: publish.source === (window as unknown as { selectedFile: File }).selectedFile,
				};
			}),
		).toEqual({ enabled: false, announced: "false", catalog: "manual", source: "file", sameFile: true });
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
			for (const value of ["moq-transport-14", " moq-lite-06 , moq-transport-22 ", "", " , ", null]) {
				if (value === null) watch.removeAttribute("versions");
				else watch.setAttribute("versions", value);
				versions.push(watch.versions);
			}
			watch.setAttribute("announced", "false");
			const announced = watch.announced;
			watch.signals.close();
			return { versions, announced };
		});
		expect(result).toEqual({
			versions: [["moq-transport-14"], ["moq-lite-06", "moq-transport-22"], undefined, undefined, undefined],
			announced: false,
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
				const list = ["moq-lite-06", "moq-transport-14"];
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
				snapshot: ["moq-lite-06", "moq-transport-14"],
				reflected: "moq-lite-06,moq-transport-14",
			});
	} finally {
		await browser.close();
		await server.close();
	}
}, 30_000);
