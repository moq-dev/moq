// The "another bundler" variant: bundle the same entry point with esbuild
// instead of vite, to confirm the published packages consume cleanly under a
// different bundler. Writes dist-esbuild/{main.js,index.html}.
import { mkdirSync, writeFileSync } from "node:fs";
import { build } from "esbuild";

await build({
	entryPoints: ["src/main.ts"],
	bundle: true,
	format: "esm",
	target: "esnext",
	outfile: "dist-esbuild/main.js",
	logLevel: "info",
});

mkdirSync("dist-esbuild", { recursive: true });
writeFileSync(
	"dist-esbuild/index.html",
	`<!doctype html>
<html>
  <head><meta charset="utf-8" /><title>moq smoke (esbuild)</title></head>
  <body><script type="module" src="./main.js"></script></body>
</html>
`,
);
