// Main PRs run in the isolated batched gate; release PRs retain their own checks.
import assert from "node:assert/strict";
import { mkdir, mkdtemp, readdir, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";

const workflow = async (name: string) => Bun.YAML.parse(await Bun.file(`.github/workflows/${name}.yml`).text());
const check = await workflow("check");
const land = await workflow("land");
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
// Release's selector and execution share one job-wide base; Land uses its
// resolved main SHA for every checked prefix, never the preceding PR's head.
assert.equal(check.jobs.check.env.CI_BASE, "--all");
// Over every package, the isolated feature shapes outgrow Check's job; Features runs them.
assert.equal(check.jobs.check.env.CHECK_MODULES, "-rs_shapes");
for (const step of check.jobs.check.steps) assert.equal(step.env?.CI_BASE, undefined, `${step.name} overrides CI_BASE`);
// biome-ignore lint/suspicious/noTemplateCurlyInString: GitHub expression is literal YAML.
assert.equal(check.concurrency.group, "check-${{ github.ref }}");
assert.equal(check.concurrency["cancel-in-progress"], true);

// The plan is resolved on hosted infrastructure. PR code sees only a read
// credential; a separate hosted job rebuilds the chain before pushing it.
assert.deepEqual(land.permissions, {});
assert.equal(land.concurrency.group, "land");
assert.equal(land.concurrency["cancel-in-progress"], false);
assert.deepEqual(land.on.push.branches, ["main"]);
for (const event of ["auto_merge_enabled", "auto_merge_disabled", "synchronize"]) {
	assert.ok(land.on.pull_request_target.types.includes(event));
}
assert.equal(land.jobs.resolve["runs-on"], "ubuntu-latest");
assert.equal(land.jobs.land["runs-on"], "ubuntu-latest");
assert.equal(land.jobs.resolve.permissions.contents, "read");
assert.deepEqual(land.jobs.check.permissions, { contents: "read" });
assert.equal(land.jobs.check.environment, undefined);
// token.moq.dev issues moq-bot's token to any job of main's land.yml that asks
// with an ID token, so only the land job, which runs no queued PR code, may.
assert.equal(land.jobs.land.environment, undefined);
assert.equal(land.permissions?.["id-token"], undefined);
for (const [id, job] of Object.entries(land.jobs) as [string, { uses?: string; permissions?: unknown }][]) {
	const grants =
		job.permissions === "write-all" || (job.permissions as Record<string, string>)?.["id-token"] === "write";
	assert.equal(grants, id === "land", `land.yml/${id} must not request an ID token`);
	assert.equal(
		job.uses,
		undefined,
		`land.yml/${id} must not call a workflow, whose jobs inherit land.yml's workflow_ref`,
	);
}
assert.ok(!JSON.stringify(land.jobs.land).includes("APP_PRIVATE_KEY"));
assert.deepEqual(land.jobs.land.needs, ["resolve", "check"]);
// The self-hosted worker has no jq; the hosted resolver passes it the base.
assert.ok(!JSON.stringify(land.jobs.check).includes("jq "), "the check job must not need jq");
// biome-ignore lint/suspicious/noTemplateCurlyInString: GitHub expression is literal YAML.
assert.equal(land.jobs.resolve.outputs.base, "${{ steps.resolve.outputs.base }}");
for (const job of Object.values(land.jobs) as { steps: { uses?: string; with?: { ref?: string } }[] }[]) {
	const checkout = job.steps.find((step) => step.uses?.startsWith("actions/checkout@"));
	assert.equal(checkout?.with?.ref, "main", "every phase installs Quest from trusted main");
}
assert.ok(!JSON.stringify(land.jobs.check).includes("APP_PRIVATE_KEY"));
assert.ok(land.jobs.land.steps.some((step: { run?: string }) => step.run?.includes("quest land push")));

// A failed engine process must fail its job, rather than publish an empty
// successful output. No phase in this fixture touches GitHub or a real ref.
await mkdir(".tmp", { recursive: true });
const scratch = await mkdtemp(join(process.cwd(), ".tmp/land-policy-"));
try {
	const bin = join(scratch, "bin");
	await mkdir(bin);
	await writeFile(
		join(bin, "quest"),
		'#!/bin/sh\nprintf "%s\\n" "$@" > "$ENGINE_ARGS"\nprintf "%s\\n" "$ENGINE_OUTPUT"\nexit "$ENGINE_EXIT"\n',
		{
			mode: 0o755,
		},
	);
	await writeFile(join(bin, "git"), "#!/bin/sh\nexit 0\n", { mode: 0o755 });
	for (const [job, id] of [
		["resolve", "resolve"],
		["check", "check"],
	]) {
		const script = land.jobs[job].steps.find((step: { id?: string }) => step.id === id).run;
		for (const code of [0, 33]) {
			const output = join(scratch, `${job}-${code}`);
			const result = Bun.spawnSync(["bash", "-euo", "pipefail", "-c", script], {
				cwd: scratch,
				env: {
					...process.env,
					PATH: `${bin}:${process.env.PATH}`,
					PLAN: JSON.stringify({ base: { sha: "a".repeat(40) } }),
					BASE: "a".repeat(40),
					GITHUB_OUTPUT: output,
					ENGINE_EXIT: String(code),
					ENGINE_OUTPUT: JSON.stringify({ base: { sha: "a".repeat(40) }, checks: [] }),
					ENGINE_ARGS: join(scratch, "args"),
				},
			});
			assert.equal(result.exitCode, code, `${job} must propagate engine failure`);
			const posted = await Bun.file(output).exists();
			assert.equal(posted, code === 0, `${job} must not publish success after failure`);
			const args = (await Bun.file(join(scratch, "args")).text()).trim().split("\n");
			assert.deepEqual(
				args,
				job === "resolve"
					? ["land", "resolve", "--base", "main"]
					: [
							"land",
							"check",
							".tmp/land/plan.json",
							"--budget",
							"150",
							"--",
							"nix",
							"develop",
							"--command",
							"just",
							"ci",
							"a".repeat(40),
						],
			);
		}
	}
} finally {
	await rm(scratch, { recursive: true, force: true });
}

// Every other workflow stays out of main PRs and main's merge groups: optional
// checks run nightly on main, and on release PRs through the Release gate.
const files = (await readdir(".github/workflows")).filter((file) => file.endsWith(".yml"));
for (const file of files) {
	const name = file.replace(/\.yml$/, "");
	if (name === "check" || name === "release-gate") continue;
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
assert.ok(enqueue.run.includes('GH_TOKEN="$READ_TOKEN" quest land resolve --base main'));
// The resolver reads check results, which moq-bot's installation cannot.
assert.equal(request.permissions.checks, "read");
assert.equal(request.permissions.statuses, "read");
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
	"wasm",
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
for (const name of ["x11", "android", "cpp", "wasm", "audio-quality", "oss-scanner", "runner-vm", "features"]) {
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

// Neither the feature matrix nor the optimized WASM build shares the queue's
// default-feature test artifacts, but both must block a release.
const features = await workflow("features");
assert.deepEqual(features.jobs.features.strategy.matrix.recipe, ["rs features", "gate rs_shapes", "js wasm"]);
// Nightly's gate splits across Tests and Features, which together run all of it.
assert.ok(nightly.jobs.tests.strategy.matrix.recipe.includes("gate -rs_shapes"));
assert.ok(!nightly.jobs.tests.strategy.matrix.recipe.includes("gate"));
console.log("queue event policy passed");
