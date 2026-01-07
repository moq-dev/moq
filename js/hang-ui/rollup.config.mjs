import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import nodeResolve from "@rollup/plugin-node-resolve";
import cssnano from "cssnano";
import { globSync } from "glob";
import postcss from "postcss";
import postcssImport from "postcss-import";
import esbuild from "rollup-plugin-esbuild";
import { optimize as optimizeSvg } from "svgo";
import solid from "unplugin-solid/rollup";

// Plugin to bundle and minify CSS, and optimize SVGs
function processAssets() {
	return {
		name: "process-assets",
		async writeBundle() {
			// Process CSS files - bundle @imports and minify
			const cssFiles = globSync("src/assets/styles/**/*.css");
			for (const file of cssFiles) {
				const content = readFileSync(file, "utf8");
				const result = await postcss([
					postcssImport({
						path: [dirname(file)], // Resolve @imports relative to the file
					}),
					cssnano({ preset: "default" }),
				]).process(content, {
					from: file,
				});

				const outPath = file.replace("src/assets/", "dist/assets/");
				mkdirSync(dirname(outPath), { recursive: true });
				writeFileSync(outPath, result.css);
			}

			// Process SVG files - optimize
			const svgFiles = globSync("src/assets/icons/**/*.svg");
			for (const file of svgFiles) {
				const content = readFileSync(file, "utf8");
				const result = optimizeSvg(content, {
					multipass: true,
					plugins: ["preset-default", { name: "removeViewBox", active: false }],
				});

				const outPath = file.replace("src/assets/", "dist/assets/");
				mkdirSync(dirname(outPath), { recursive: true });
				writeFileSync(outPath, result.data);
			}
		},
	};
}

// Shared plugins for Solid components
const solidPlugins = [
	solid({ dev: false, hydratable: false }),
	esbuild({
		include: /\.[jt]sx?$/,
		jsx: "preserve",
		tsconfig: "tsconfig.json",
	}),
	nodeResolve({ extensions: [".js", ".ts", ".tsx"] }),
];

// Simple esbuild plugin for non-Solid files
const simplePlugins = [
	esbuild({
		include: /\.[jt]s$/,
		tsconfig: "tsconfig.json",
	}),
	nodeResolve({ extensions: [".js", ".ts"] }),
];

export default [
	{
		input: "src/index.ts",
		output: { file: "dist/index.js", format: "es" },
		plugins: simplePlugins,
	},
	{
		input: "src/settings.ts",
		output: { file: "dist/settings.js", format: "es" },
		plugins: simplePlugins,
	},
	{
		input: "src/Components/publish/element.tsx",
		output: { file: "dist/Components/publish/element.js", format: "es", sourcemap: true },
		plugins: solidPlugins,
	},
	{
		input: "src/Components/watch/element.tsx",
		output: { file: "dist/Components/watch/element.js", format: "es", sourcemap: true },
		plugins: solidPlugins,
	},
	{
		input: "src/Components/stats/index.ts",
		output: { file: "dist/Components/stats/index.js", format: "es", sourcemap: true },
		plugins: solidPlugins,
	},
];
