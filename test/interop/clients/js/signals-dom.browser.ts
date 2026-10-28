import { expect, test } from "bun:test";
import type { Effect } from "@moq/signals";
import { launch } from "./harness";

const bundle = await Bun.build({
	entrypoints: [`${import.meta.dir}/../../../../js/signals/src/dom.ts`],
	target: "browser",
});
if (!bundle.success) throw new AggregateError(bundle.logs, "could not bundle signals DOM helpers");
const source = `data:text/javascript;base64,${Buffer.from(await bundle.outputs[0].text()).toString("base64")}`;

test("creating an element without options keeps its children", async () => {
	const browser = await launch();
	try {
		const page = await browser.newPage();
		const result = await page.evaluate(async (source) => {
			const DOM = (await import(source)) as typeof import("@moq/signals/dom");
			const child = document.createElement("b");
			child.textContent = "child";
			const parent = DOM.create("div", undefined, "text", child);
			return { text: parent.textContent, ownsChild: child.parentNode === parent };
		}, source);
		expect(result).toEqual({ text: "textchild", ownsChild: true });
	} finally {
		await browser.close();
	}
});

test("rendered nested arrays clean up their nodes and preserve existing siblings", async () => {
	const browser = await launch();
	try {
		const page = await browser.newPage();
		const result = await page.evaluate(async (source) => {
			const DOM = (await import(source)) as typeof import("@moq/signals/dom");
			const cleanups: (() => void)[] = [];
			const effect = { cleanup: (cleanup: () => void) => cleanups.push(cleanup) } as unknown as Effect;
			const parent = document.createElement("div");
			parent.textContent = "existing";
			const child = document.createElement("b");
			child.textContent = "child";
			const fragment = document.createDocumentFragment();
			fragment.append("fragment");
			DOM.render(effect, parent, ["text", [child, 0, false], fragment, null, undefined]);
			const rendered = parent.textContent;
			for (const cleanup of cleanups.reverse()) cleanup();
			return { rendered, remaining: parent.textContent, childDetached: child.parentNode === null };
		}, source);
		expect(result).toEqual({
			rendered: "existingtextchild0falsefragment",
			remaining: "existing",
			childDetached: true,
		});
	} finally {
		await browser.close();
	}
});
