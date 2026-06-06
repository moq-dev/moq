// Print the absolute path to the published @moq/token CLI bin, for both node
// and bun. We read package.json off disk instead of via module resolution
// because @moq/token's exports map doesn't expose ./package.json, which node's
// strict ESM resolver refuses (bun allows it); reading the file keeps both
// runtimes on the same path while still taking the bin name from the manifest.
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
