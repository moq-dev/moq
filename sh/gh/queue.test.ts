// Main PRs land through GitHub's merge queue (queue.yml); release PRs retain their own checks.
import assert from "node:assert/strict";
import { readdir } from "node:fs/promises";

const workflow = async (name: string) => Bun.YAML.parse(await Bun.file(`.github/workflows/${name}.yml`).text());
const check = await workflow("check");
const gate = await workflow("release-gate");
const platform = await workflow("platform");
const dart = await workflow("release-dart-ffi");
const nightly = await workflow("nightly");

function condition(expression: string, event: string, base: string, selected = true, action = "opened"): unknown {
	const source = expression.replace(/^\$\{\{\s*|\s*\}\}$/g, "");
	const github = { event_name: event, base_ref: base, event: { action } };
	const outputs = Object.fromEntries(
		["needed", "rs", "swift", "cpp", "obs", "moq_sh"].map((key) => [key, String(selected)]),
	);
	return new Function("github", "needs", `return (${source});`)(github, { scope: { outputs } });
}

// Check stays reportable on main PRs, but only release PRs execute it.
assert.equal(check.on.merge_group, undefined);
assert.equal(check.on.pull_request.paths, undefined, "required PR contexts must always report");
assert.equal(check.on.pull_request.branches, undefined, "main PRs must report the required names");
assert.ok(check.on.pull_request.types.includes("edited"), "retargeting to release must rerun");
for (const job of [check.jobs.check]) {
	assert.equal(condition(job.if, "pull_request", "main"), false);
	assert.equal(condition(job.if, "pull_request", "release"), true);
	assert.equal(condition(job.if, "pull_request", "release", true, "edited"), true);
	assert.equal(condition(job.if, "pull_request", "release", true, "closed"), false);
}
// Release's selector and execution share one job-wide base.
assert.equal(check.jobs.check.env.CI_BASE, "--all");
// Check runs the whole gate in one job.
assert.equal(check.jobs.check.env.CHECK_MODULES, undefined);
for (const step of check.jobs.check.steps) assert.equal(step.env?.CI_BASE, undefined, `${step.name} overrides CI_BASE`);
// biome-ignore lint/suspicious/noTemplateCurlyInString: GitHub expression is literal YAML.
assert.equal(check.concurrency.group, "check-${{ github.ref }}");
assert.equal(check.concurrency["cancel-in-progress"], true);

// No workflow lands around the merge queue.
assert.equal(await Bun.file(".github/workflows/land.yml").exists(), false, "no workflow may land around the merge queue");

// Queue is the one merge group build: main's ruleset requires its context, so
// it reports (skipped) on main PRs and runs only in the merge queue, on the
// host's runner, read-only and without secrets.
const queue = await workflow("queue");
assert.deepEqual(queue.on.merge_group, { types: ["checks_requested"] });
assert.deepEqual(queue.on.pull_request.branches, ["main"]);
assert.equal(queue.on.pull_request.paths, undefined, "the required context must always report");
assert.deepEqual(queue.permissions, { contents: "read" });
assert.equal(queue.jobs.queue.name, "Queue");
assert.equal(queue.jobs.queue.if, "github.event_name == 'merge_group'");
assert.deepEqual(queue.jobs.queue["runs-on"], ["self-hosted", "moq-queue"]);
assert.equal(queue.jobs.queue.permissions, undefined, "the job keeps the workflow's read-only token");
assert.ok(!JSON.stringify(queue).includes("secrets."), "the merge queue build gets no secrets");
const checkout = queue.jobs.queue.steps.find((step: { uses?: string }) => step.uses?.startsWith("actions/checkout@"));
assert.equal(checkout.with["persist-credentials"], false);
// biome-ignore lint/suspicious/noTemplateCurlyInString: GitHub expression is literal YAML.
assert.equal(queue.jobs.queue.env.BASE, "${{ github.event.merge_group.base_sha }}");

// Every other workflow stays out of main PRs and main's merge groups: optional
// checks run nightly on main, and on release PRs through the Release gate.
const files = (await readdir(".github/workflows")).filter((file) => file.endsWith(".yml"));
for (const file of files) {
	const name = file.replace(/\.yml$/, "");
	if (name === "check" || name === "release-gate" || name === "queue") continue;
	const config = await workflow(name);
	assert.equal(config.on.merge_group, undefined, `${file} must not run in the merge queue`);
	assert.equal(config.on.pull_request, undefined, `${file} must run on release PRs through release-gate.yml`);
	for (const [id, job] of Object.entries(config.jobs ?? {})) {
		if (JSON.stringify(job).includes("APP_PRIVATE_KEY")) {
			// `bot` holds only the key; `publish`, for release refs alone, also
			// holds it for jobs that need it beside a registry credential.
			assert.ok(
				["bot", "publish"].includes((job as { environment?: string }).environment ?? ""),
				`${file}/${id} must scope the bot key`,
			);
		}
	}
}

const dependabot = await workflow("dependabot");
const request = dependabot.jobs["request-landing"];
const enqueue = request.steps.find((step: { name?: string }) => step.name === "Request checked landing");
// biome-ignore lint/suspicious/noTemplateCurlyInString: GitHub expression is literal YAML.
assert.equal(enqueue.env.PR_HEAD, "${{ github.event.pull_request.head.sha }}");
assert.ok(enqueue.run.includes('quest merge "$PR_URL" "$hold" --land "$PR_HEAD"'));
// Requesting a landing only pins the head; the host gate reads the queue itself.
assert.ok(!enqueue.run.includes("quest land"), "Dependabot must not run the gate's own commands");
assert.ok(!JSON.stringify(request).includes("gh pr merge"));

// Release PRs report one context for every optional workflow, retargeted PRs included.
assert.deepEqual(Object.keys(gate.on), ["pull_request"]);
assert.deepEqual(gate.on.pull_request.branches, ["release"]);
assert.equal(gate.on.pull_request.paths, undefined, "the required release PR context must always report");
assert.ok(gate.on.pull_request.types.includes("edited"), "retargeting to release must rerun");
assert.ok(gate.on.pull_request.types.includes("closed"), "closing must cancel an in-flight run");

const calls = Object.entries(gate.jobs).filter(([, job]) => (job as { uses?: string }).uses) as [
	string,
	{ uses: string; if: string },
][];
for (const [id, job] of calls) {
	const called = await workflow(job.uses.replace(/^\.\/\.github\/workflows\/|\.yml$/g, ""));
	assert.ok("workflow_call" in called.on, `${job.uses} must be callable`);
	assert.equal(
		id,
		job.uses.replace(/^\.\/\.github\/workflows\/|\.yml$/g, ""),
		`${id} must be named after its workflow`,
	);
}
// Every workflow that used to run on release PRs blocks a release now.
assert.deepEqual(calls.map(([id]) => id).sort(), [
	"android",
	"api-diff",
	"audio-quality",
	"c",
	"cpp",
	"features",
	"interop",
	"moq-sh",
	"obs",
	"oss-scanner",
	"platform",
	"release-dart",
	"release-dart-ffi",
	"release-go-ffi",
	"release-js",
	"release-kt-lib",
	"release-swift-lib",
	"runner-vm",
	"ts",
	"uring",
	"video",
	"x11",
]);

// Platform and Features select their own work; every other call runs when its filter matches.
const filters = Object.keys(Bun.YAML.parse(gate.jobs.scope.steps.at(-1).with.filters) as object);
const unfiltered = ["features", "platform"];
for (const [id, job] of calls) {
	if (unfiltered.includes(id)) {
		assert.equal(condition(job.if, "pull_request", "release"), true);
		assert.equal(condition(job.if, "pull_request", "release", true, "closed"), false);
		continue;
	}
	assert.equal(job.if, `needs.scope.outputs.${id} == 'true'`);
	assert.equal(gate.jobs.scope.outputs[id], `\${{ steps.filter.outputs.${id} }}`);
}
assert.deepEqual(
	filters.sort(),
	calls
		.map(([id]) => id)
		.filter((id) => !unfiltered.includes(id))
		.sort(),
);
assert.deepEqual(Object.keys(gate.jobs.scope.outputs).sort(), filters.sort());
// The REST file list stops at 3,000 files, so the filter diffs with git.
assert.equal(gate.jobs.scope.steps.at(-1).with.token, "");
assert.equal(gate.jobs.scope.steps[0].with["fetch-depth"], 0);

// The gate fails on any failure or cancellation, a failed scope included, and
// passes skipped workflows. It runs even in a cancelled run, so an `edited`
// rerun never leaves a skipped, passing context on the commit.
const release = gate.jobs.gate;
assert.equal(release.name, "Release gate");
assert.deepEqual(
	[...release.needs].sort(),
	Object.keys(gate.jobs)
		.filter((id) => id !== "gate")
		.sort(),
);
function verdict(expression: string, action: string, results: string[]): unknown {
	const source = expression
		.replace(/always\(\)/g, "true")
		.replace(/contains\(needs\.\*\.result, '(\w+)'\)/g, "results.includes('$1')");
	return new Function("github", "results", `return (${source});`)({ event: { action } }, results);
}
assert.equal(verdict(release.if, "synchronize", ["cancelled"]), true);
assert.equal(verdict(release.if, "closed", ["success"]), false);
const fail = release.steps.find((step: { run?: string }) => step.run === "exit 1");
for (const [results, failed] of [
	[["success", "skipped"], false],
	[["success", "failure"], true],
	[["skipped", "cancelled"], true],
] as const) {
	assert.equal(verdict(fail.if, "synchronize", [...results]), failed, `gate over ${results}`);
}

// Platform selects its native suites from the diff, and Nightly runs it on main.
assert.deepEqual(Object.keys(platform.jobs).sort(), ["macos", "scope", "windows"]);
// The selector runs before any toolchain, through the same query as Check.
assert.equal(platform.jobs.scope.steps.at(-1).run, 'just ci-select-platform "$CI_BASE" >> "$GITHUB_OUTPUT"');
for (const job of [platform.jobs.macos, platform.jobs.windows]) {
	assert.equal(condition(job.if, "pull_request", "release", false), false);
	assert.equal(condition(job.if, "pull_request", "release", true), true);
}
assert.equal(nightly.jobs.platform.uses, "./.github/workflows/platform.yml");
assert.deepEqual(platform.on.push.branches, ["release"]);

// Nightly covers what main PRs skip.
for (const name of ["x11", "android", "cpp", "audio-quality", "oss-scanner", "runner-vm", "features"]) {
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

assert.deepEqual((await workflow("obs")).on.push.branches, ["release"]);

// Release Dart FFI only runs on release PRs and nightly, so it packages every platform.
const targets = JSON.parse(dart.jobs.ffi.with.targets);
assert.ok(targets.some((target: string) => target.includes("-apple-")));
assert.ok(targets.some((target: string) => target.includes("-windows-")));

// The feature matrix shares none of the queue's default-feature test artifacts, but
// it must block a release.
const features = await workflow("features");
assert.deepEqual(features.jobs.features.strategy.matrix.recipe, ["rs features"]);
// Nightly runs the whole gate over every package in one Tests job.
assert.ok(nightly.jobs.tests.strategy.matrix.recipe.includes("gate"));
console.log("queue event policy passed");
