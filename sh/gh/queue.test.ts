// Exercise the event policy: skipped native PR contexts, one OS job per merge
// group, and release PR coverage despite release having no merge queue.
import assert from "node:assert/strict";

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

for (const config of [check, platform]) {
	assert.ok(config.on.pull_request);
	assert.equal(config.on.pull_request.paths, undefined, "required PR contexts must always report");
	assert.ok(config.on.merge_group);
}
// Check and Test run on every PR as well as in the queue.
for (const job of [check.jobs.check, check.jobs.test]) {
	for (const base of ["main", "release"]) assert.equal(condition(job.if, "pull_request", base), true);
	assert.equal(condition(job.if, "merge_group", ""), true);
}
// Native jobs skip main PRs; release has no queue, so its PRs run them.
assert.equal(condition(platform.jobs.scope.if, "pull_request", "main"), false);
assert.equal(condition(platform.jobs.scope.if, "pull_request", "release"), true);
assert.equal(condition(platform.jobs.scope.if, "merge_group", ""), true);
// Retargeting a main PR to release emits `edited`, which must run the native gates.
assert.equal(condition(platform.jobs.scope.if, "pull_request", "release", true, "edited"), true);
// Check, Test and Release Dart FFI already run on main PRs, where an edit would
// only cancel and repeat their builds.
for (const config of [platform, swift]) {
	assert.ok(config.on.pull_request.types.includes("edited"), "retargeting to release must rerun");
}
assert.deepEqual(Object.keys(platform.jobs).sort(), ["macos", "scope", "windows"]);
// The selector runs before any toolchain, through the same query as Check and Test.
assert.equal(platform.jobs.scope.steps.at(-1).run, 'just ci-select platform "$CI_BASE" >> "$GITHUB_OUTPUT"');
for (const job of [platform.jobs.macos, platform.jobs.windows]) {
	assert.equal(condition(job.if, "merge_group", "", false), false);
	assert.equal(condition(job.if, "merge_group", "", true), true);
}
assert.equal((await workflow("obs")).jobs.release, undefined);
assert.equal((await workflow("cpp")).jobs.msvc, undefined);
assert.deepEqual((await workflow("moq-sh")).jobs.test.strategy.matrix.os, ["ubuntu-latest"]);
assert.equal(await Bun.file(".github/workflows/swift.yml").exists(), false);
assert.deepEqual(swift.on.pull_request.branches, ["release"]);
assert.ok(nightly.jobs["release-swift-lib"]);
assert.equal(nightly.jobs.platform.uses, "./.github/workflows/platform.yml");
assert.deepEqual(platform.on.push.branches, ["release"]);
const targets = dart.jobs.ffi.with.targets;
assert.ok(
	JSON.parse(condition(targets, "pull_request", "main") as string).every(
		(target: string) => !/-apple-|-windows-/.test(target),
	),
);
for (const [event, base] of [
	["pull_request", "release"],
	["schedule", ""],
]) {
	assert.ok(
		JSON.parse(condition(targets, event, base) as string).some((target: string) => target.includes("-apple-")),
	);
	assert.ok(
		JSON.parse(condition(targets, event, base) as string).some((target: string) => target.includes("-windows-")),
	);
}
console.log("queue event policy passed");
