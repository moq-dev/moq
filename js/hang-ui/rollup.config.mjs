import { readFileSync } from "node:fs";
import nodeResolve from "@rollup/plugin-node-resolve";
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

export default [
	{
		input: "src/Components/publish/element.tsx",
		output: {
			file: "dist/publish-controls.esm.js",
			format: "es",
			sourcemap: true,
		},
		plugins: [
			inlineCss(),
			solid({ dev: false, hydratable: false }),
			esbuild({
				include: /\.[jt]sx?$/,
				jsx: "preserve",
				tsconfig: "tsconfig.json",
			}),
			nodeResolve({ extensions: [".js", ".ts", ".tsx"] }),
		],
	},
	{
		input: "src/Components/watch/element.tsx",
		output: {
			file: "dist/watch-controls.esm.js",
			format: "es",
			sourcemap: true,
		},
		plugins: [
			inlineCss(),
			solid({ dev: false, hydratable: false }),
			esbuild({
				include: /\.[jt]sx?$/,
				jsx: "preserve",
				tsconfig: "tsconfig.json",
			}),
			nodeResolve({ extensions: [".js", ".ts", ".tsx"] }),
		],
	},
];
