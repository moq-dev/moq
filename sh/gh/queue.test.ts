// Exercise the event policy: main PRs run no cloud CI, the merge queue runs only
// Check and Test, and release PRs keep everything since release has no queue.
import assert from "node:assert/strict";
import { readdir } from "node:fs/promises";

const workflow = async (name: string) => Bun.YAML.parse(await Bun.file(`.github/workflows/${name}.yml`).text());
const check = await workflow("check");
const platform = await workflow("platform");
const dart = await workflow("release-dart-ffi");
const swift = await workflow("release-swift-lib");
const nightly = await workflow("nightly");

function condition(expression: string, event: string, base: string, selected = true, action = "opened"): unknown {
	const source = expression.replace(/^\$\{\{\s*|\s*\}\}$/g, "");
	const github = { event_name: event, base_ref: base, event: { action } };
	const outputs = Object.fromEntries(
		["needed", "rs", "swift", "cpp", "obs", "moq_sh"].map((key) => [key, String(selected)]),
	);
	return new Function("github", "needs", `return (${source});`)(github, { scope: { outputs } });
}

// Check and Test are main's only queue gate. Every PR reports both names, since
// they are required, but only release PRs and merge groups do the work.
assert.ok(check.on.merge_group);
assert.equal(check.on.pull_request.paths, undefined, "required PR contexts must always report");
assert.equal(check.on.pull_request.branches, undefined, "main PRs must report the required names");
assert.ok(check.on.pull_request.types.includes("edited"), "retargeting to release must rerun");
for (const job of [check.jobs.check, check.jobs.test]) {
	assert.equal(condition(job.if, "pull_request", "main"), false);
	assert.equal(condition(job.if, "pull_request", "release"), true);
	assert.equal(condition(job.if, "pull_request", "release", true, "edited"), true);
	assert.equal(condition(job.if, "pull_request", "release", true, "closed"), false);
	assert.equal(condition(job.if, "merge_group", ""), true);
}

// Every other workflow stays out of main PRs and main's merge groups: optional
// checks run nightly on main and on release PRs, retargeted ones included.
const files = (await readdir(".github/workflows")).filter((file) => file.endsWith(".yml"));
for (const file of files) {
	const name = file.replace(/\.yml$/, "");
	if (name === "check") continue;
	const config = await workflow(name);
	assert.equal(config.on.merge_group, undefined, `${file} must not run in the merge queue`);
	if (!config.on.pull_request) continue;
	assert.deepEqual(config.on.pull_request.branches, ["release"], `${file} must skip main PRs`);
	assert.ok(config.on.pull_request.types.includes("edited"), `${file} must rerun when retargeted to release`);
}

// Platform reports on every release PR without path filters, so release can
// require its names, and Nightly runs it on main.
assert.equal(platform.on.pull_request.paths, undefined, "required release PR contexts must always report");
assert.equal(condition(platform.jobs.scope.if, "pull_request", "release"), true);
assert.equal(condition(platform.jobs.scope.if, "pull_request", "release", true, "edited"), true);
assert.equal(condition(platform.jobs.scope.if, "schedule", ""), true);
assert.deepEqual(Object.keys(platform.jobs).sort(), ["macos", "scope", "windows"]);
// The selector runs before any toolchain, through the same query as Check and Test.
assert.equal(platform.jobs.scope.steps.at(-1).run, 'just ci-select platform "$CI_BASE" >> "$GITHUB_OUTPUT"');
for (const job of [platform.jobs.macos, platform.jobs.windows]) {
	assert.equal(condition(job.if, "pull_request", "release", false), false);
	assert.equal(condition(job.if, "pull_request", "release", true), true);
}
assert.equal(nightly.jobs.platform.uses, "./.github/workflows/platform.yml");
assert.deepEqual(platform.on.push.branches, ["release"]);

// Nightly covers what main PRs skip.
for (const name of ["x11", "android", "cpp", "wasm", "audio-quality", "oss-scanner", "runner-vm"]) {
	assert.ok(
		Object.values(nightly.jobs).some(
			(job) => (job as { uses?: string }).uses === `./.github/workflows/${name}.yml`,
		),
		`nightly must call ${name}.yml`,
	);
}
for (const job of ["release-swift-lib", "release-dart-ffi", "release-dart", "release-js", "release-kt-lib"]) {
	assert.ok(nightly.jobs[job], `nightly must dry-run ${job}`);
}

assert.equal((await workflow("obs")).jobs.release, undefined);
assert.deepEqual((await workflow("obs")).on.push.branches, ["release"]);
assert.equal((await workflow("cpp")).jobs.msvc, undefined);
assert.deepEqual((await workflow("moq-sh")).jobs.test.strategy.matrix.os, ["ubuntu-latest"]);
assert.equal(await Bun.file(".github/workflows/swift.yml").exists(), false);
assert.deepEqual(swift.on.pull_request.branches, ["release"]);

// Release Dart FFI only runs on release PRs and nightly, so it packages every platform.
const targets = JSON.parse(dart.jobs.ffi.with.targets);
assert.ok(targets.some((target: string) => target.includes("-apple-")));
assert.ok(targets.some((target: string) => target.includes("-windows-")));
console.log("queue event policy passed");
