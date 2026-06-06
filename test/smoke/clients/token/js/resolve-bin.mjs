// Print the absolute path to the published @moq/token CLI entrypoint.
//
// token.sh runs this with BOTH node and bun so each runtime resolves the same
// installed package and we drive the *published* bin (compiled dist), not the
// in-tree TypeScript source. We read the installed package.json straight off
// disk rather than via module resolution: @moq/token's `exports` map doesn't
// expose ./package.json, which Node's strict ESM resolver refuses (bun allows
// it), so require.resolve would work under bun but throw under node. Reading the
// file keeps both runtimes on the same path, and the bin name is still taken
// from the published manifest so a rename surfaces here.
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const pkgDir = resolve(process.cwd(), "node_modules/@moq/token");
const pkg = JSON.parse(readFileSync(resolve(pkgDir, "package.json"), "utf8"));

const bin = typeof pkg.bin === "string" ? pkg.bin : (pkg.bin?.["moq-token"] ?? pkg.bin?.["moq-token-cli"]);
if (!bin) {
	console.error("@moq/token exposes no moq-token bin; published package changed shape");
	process.exit(1);
}

console.log(resolve(pkgDir, bin));
