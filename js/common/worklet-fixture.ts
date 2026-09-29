import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { build } from "vite";
import { worklet } from "./vite-plugin-worklet";

/** Build a worklet library, then consume its emitted URLs in an ordinary Vite app. */
export async function workletFixture(script = "") {
	const scratch = resolve(import.meta.dir, "../../.scratch");
	mkdirSync(scratch, { recursive: true });
	const root = mkdtempSync(join(scratch, "worklet-"));
	const entry = join(root, "library.ts");
	writeFileSync(
		entry,
		`export {default as render} from ${JSON.stringify(resolve(import.meta.dir, "../watch/src/audio/render-worklet.ts?worklet"))};\nexport {default as capture} from ${JSON.stringify(resolve(import.meta.dir, "../publish/src/audio/capture-worklet.ts?worklet"))};`,
	);
	const close = () => rmSync(root, { recursive: true, force: true });
	try {
		await build({
			configFile: false,
			root,
			logLevel: "silent",
			plugins: [worklet()],
			build: {
				outDir: "node_modules/worklet-fixture",
				lib: { entry, formats: ["es"], fileName: () => "index.js" },
			},
		});
		writeFileSync(
			join(root, "node_modules/worklet-fixture/package.json"),
			JSON.stringify({ name: "worklet-fixture", type: "module", exports: "./index.js" }),
		);
		writeFileSync(
			join(root, "index.html"),
			'<button id="start">Start audio</button><script type="module" src="/app.js"></script>',
		);
		writeFileSync(
			join(root, "app.js"),
			`import {render, capture} from 'worklet-fixture'; window.worklets = {render, capture};\n${script}`,
		);
		await build({ configFile: false, root, logLevel: "silent", build: { outDir: "app" } });
		return { root, close };
	} catch (error) {
		close();
		throw error;
	}
}
