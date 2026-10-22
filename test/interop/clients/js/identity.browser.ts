import { expect, test } from "bun:test";
import type { Vectors } from "../../../../js/net/src/lite/identity.fixture";
import { launch } from "./harness";

const bundle = await Bun.build({
	entrypoints: [`${import.meta.dir}/../../../../js/net/src/lite/identity.fixture.ts`],
	target: "browser",
});
if (!bundle.success) throw new AggregateError(bundle.logs, "could not bundle identity fixture");
const source = `data:text/javascript;base64,${Buffer.from(await bundle.outputs[0].text()).toString("base64")}`;

test("unactivated identity and keyless-relay fixtures run with real browser WebCrypto", async () => {
	const vectors: Vectors = await Bun.file(new URL("../../identity/vectors.json", import.meta.url)).json();
	const server = Bun.serve({
		hostname: "127.0.0.1",
		port: 0,
		fetch: () =>
			new Response("<!doctype html><title>Identity fixture</title>", {
				headers: { "Content-Type": "text/html" },
			}),
	});
	let browser: Awaited<ReturnType<typeof launch>> | undefined;
	try {
		browser = await launch();
		const page = await browser.newPage();
		await page.goto(server.url.href);
		const results = await page.evaluate(
			async ({ source, vectors }) => {
				if (!isSecureContext) throw new Error("WebCrypto fixture requires a secure context");
				const candidate = await import(source);
				return candidate.runIdentityCases(vectors);
			},
			{ source, vectors },
		);
		expect(results).toHaveLength(7);
	} finally {
		await browser?.close();
		await server.stop(true);
	}
}, 30_000);
