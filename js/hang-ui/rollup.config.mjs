import { readFileSync } from "node:fs";
import nodeResolve from "@rollup/plugin-node-resolve";
import copy from "rollup-plugin-copy";
import esbuild from "rollup-plugin-esbuild";
import solid from "unplugin-solid/rollup";

function inlineCss() {
	return {
		name: "inline-css",
		load(id) {
			if (id.endsWith(".css?inline")) {
				const realId = id.replace(/\?inline$/, "");
				const css = readFileSync(realId, "utf8");
				return `export default ${JSON.stringify(css)};`;
			}
		},
	};
}

// Shared plugins used by all build configs
const sharedPlugins = [
	inlineCss(),
	solid({ dev: false, hydratable: false }),
	esbuild({
		include: /\.[jt]sx?$/,
		jsx: "preserve",
		tsconfig: "tsconfig.json",
	}),
	nodeResolve({ extensions: [".js", ".ts", ".tsx"] }),
];

export default [
	{
		input: "src/utilities/index.ts",
		output: {
			file: "dist/utilities/index.js",
			format: "es",
		},
		plugins: [
			esbuild({
				include: /\.[jt]s$/,
				tsconfig: "tsconfig.json",
			}),
			copy({
				targets: [
					{ src: "src/assets/", dest: "dist/" },
					{ src: "src/themes/", dest: "dist/" },
				],
				copyOnce: true,
			}),
		],
	},
	{
		input: "src/Components/publish/element.tsx",
		output: {
			file: "dist/publish-controls.esm.js",
			format: "es",
			sourcemap: true,
		},
		plugins: sharedPlugins,
	},
	{
		input: "src/Components/watch/element.tsx",
		output: {
			file: "dist/watch-controls.esm.js",
			format: "es",
			sourcemap: true,
		},
		plugins: sharedPlugins,
	},
];
