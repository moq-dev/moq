// Main PRs land through GitHub's merge queue (queue.yml); no workflow checks a pull request.
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { readdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const workflow = async (name: string) => Bun.YAML.parse(await Bun.file(`.github/workflows/${name}.yml`).text());
const platform = await workflow("platform");
const dart = await workflow("release-dart-ffi");
const nightly = await workflow("nightly");

function condition(expression: string, event: string, selected: boolean): unknown {
	const source = expression.replace(/^\$\{\{\s*|\s*\}\}$/g, "");
	const outputs = Object.fromEntries(
		["needed", "rs", "swift", "cpp", "obs", "moq_sh"].map((key) => [key, String(selected)]),
	);
	return new Function("github", "needs", `return (${source});`)({ event_name: event }, { scope: { outputs } });
}

// No workflow lands around the merge queue.
assert.equal(
	await Bun.file(".github/workflows/land.yml").exists(),
	false,
	"no workflow may land around the merge queue",
);

// Queue is the one merge group build: main's ruleset requires the `Queue`
// context, which queue-pr.yml reports (skipped) on main PRs and queue-build.yml
// posts from the merge queue, on the host's runner, without secrets.
const queue = await workflow("queue");
// Inside the call, github.workflow is this name, which rust-cache's self-hosted guard requires.
assert.equal(queue.name, "Queue");
assert.deepEqual(Object.keys(queue.on), ["merge_group"], "a skipped job here would report on PRs and groups alike");
assert.deepEqual(queue.on.merge_group, { types: ["checks_requested"] });
assert.equal(queue.concurrency, undefined, "a cancelled group would report Queue cancelled");
assert.deepEqual(queue.permissions, { contents: "read" });
assert.deepEqual(Object.keys(queue.jobs), ["build"]);
assert.equal(queue.jobs.build.name, "Queue build");
assert.equal(
	queue.jobs.build.uses,
	"moq-dev/moq/.github/workflows/queue-build.yml@main",
	"the runner group admits only main's queue-build.yml",
);
assert.deepEqual(
	queue.jobs.build.permissions,
	{ contents: "read", checks: "write" },
	"the call grants only what the report needs",
);
assert.equal(queue.jobs.build.secrets, undefined, "the merge queue build gets no secrets");
assert.ok(!JSON.stringify(queue).includes("secrets."), "the merge queue build gets no secrets");

// queue-pr.yml reports `Queue` skipped on main PRs, and on a dispatched open
// main PR head, with nothing but `checks: write` and `pull-requests: read`.
const queuePr = await workflow("queue-pr");
assert.deepEqual(Object.keys(queuePr.on).sort(), ["pull_request", "workflow_dispatch"]);
assert.deepEqual(queuePr.on.pull_request.branches, ["main"]);
assert.ok(queuePr.on.pull_request.types.includes("edited"), "retargeting to main must report the context");
assert.equal(queuePr.on.pull_request.paths, undefined, "the required context must always report");
assert.ok(queuePr.on.workflow_dispatch.inputs.sha.required);
assert.equal(queuePr.concurrency, undefined, "a cancelled pull request run would fail the required context");
assert.deepEqual(queuePr.permissions, { contents: "read" });
assert.deepEqual(Object.keys(queuePr.jobs), ["queue"]);
assert.equal(queuePr.jobs.queue.name, "Queue");
assert.equal(queuePr.jobs.queue.if, "github.event_name == 'workflow_dispatch'");
assert.deepEqual(queuePr.jobs.queue.permissions, { checks: "write", "pull-requests": "read" });
assert.equal(queuePr.jobs.queue.steps.length, 1, "the report checks nothing out and runs no code from a PR");
assert.match(queuePr.jobs.queue.steps[0].run, /-f name=Queue -f head_sha="\$SHA"/);
// A dispatch posts only on an open main PR's head, never on a merge group's commit.
const dispatchStub = mkdtempSync(join(tmpdir(), "moq-queue-dispatch-"));
try {
	const head = "a".repeat(40);
	const pulls = join(dispatchStub, "pulls.json");
	writeFileSync(pulls, JSON.stringify([{ number: 1, head: { sha: head } }]));
	// The stub answers the open-PR listing from the fixture through the step's own filter.
	writeFileSync(
		join(dispatchStub, "gh"),
		'#!/usr/bin/env bash\nprintf "%s\\n" "$@" >> "$GH_ARGS"\n[[ "$*" == *"/pulls?state=open&base=main&"* ]] || exit 0\nwhile [[ "$1" != --jq ]]; do shift; done\njq -r "$2" "$GH_PULLS"\n',
		{ mode: 0o755 },
	);
	for (const [sha, posts] of [
		[head, true],
		["b".repeat(40), false],
	] as const) {
		const args = join(dispatchStub, "args");
		rmSync(args, { force: true });
		const run = Bun.spawnSync(
			["bash", "--noprofile", "--norc", "-eo", "pipefail", "-c", queuePr.jobs.queue.steps[0].run],
			{
				env: {
					PATH: `${dispatchStub}:${process.env.PATH}`,
					GH_ARGS: args,
					GH_PULLS: pulls,
					REPO: "o/r",
					SHA: sha,
				},
			},
		);
		assert.equal(run.exitCode === 0, posts, `${sha} exit`);
		assert.equal(readFileSync(args, "utf8").includes("repos/o/r/check-runs"), posts, `${sha} posts`);
	}
} finally {
	rmSync(dispatchStub, { recursive: true, force: true });
}

// queue-build.yml is the build queue.yml calls at main, the one workflow the
// moq-queue runner group admits. Only that call runs it, and it builds
// only in the merge queue, on the host's runner, read-only and without secrets.
const build = await workflow("queue-build");
assert.deepEqual(Object.keys(build.on), ["workflow_call"]);
assert.deepEqual(build.permissions, { contents: "read" });
assert.deepEqual(Object.keys(build.jobs), ["build", "report"]);
assert.equal(build.jobs.build.name, "Build");
assert.equal(build.jobs.build.if, "github.event_name == 'merge_group'");
assert.deepEqual(build.jobs.build["runs-on"], ["self-hosted", "moq-queue"]);
assert.equal(build.jobs.build.permissions, undefined, "the build keeps the workflow's read-only token");
assert.ok(build.jobs.build["timeout-minutes"] < 90, "a hung build must report before the queue's 90-minute timeout");
assert.ok(!JSON.stringify(build).includes("secrets."), "the merge queue build gets no secrets");
const buildCheckout = build.jobs.build.steps.find((step: { uses?: string }) =>
	step.uses?.startsWith("actions/checkout@"),
);
assert.equal(buildCheckout.with["persist-credentials"], false);
// biome-ignore lint/suspicious/noTemplateCurlyInString: GitHub expression is literal YAML.
assert.equal(build.jobs.build.env.BASE, "${{ github.event.merge_group.base_sha }}");
// The report posts `Queue` with the build's result, failed and cancelled builds
// included, and holds `checks: write` without running any of the group's code.
const report = build.jobs.report;
assert.equal(report.needs, "build");
assert.equal(report.if, "always() && github.event_name == 'merge_group'");
assert.deepEqual(report["runs-on"], ["self-hosted", "moq-queue"]);
assert.deepEqual(report.permissions, { checks: "write" });
assert.equal(report.steps.length, 1, "the report checks nothing out and runs no code from the group");
assert.equal(report.steps[0].uses, undefined);
// biome-ignore lint/suspicious/noTemplateCurlyInString: GitHub expression is literal YAML.
assert.equal(report.steps[0].env.RESULT, "${{ needs.build.result }}");
// biome-ignore lint/suspicious/noTemplateCurlyInString: GitHub expression is literal YAML.
assert.equal(report.steps[0].env.SHA, "${{ github.sha }}");
assert.match(report.steps[0].run, /-f name=Queue -f head_sha="\$SHA"/);
// Only a successful build posts a passing Queue; anything else fails the group.
const stub = mkdtempSync(join(tmpdir(), "moq-queue-report-"));
try {
	writeFileSync(join(stub, "gh"), '#!/usr/bin/env bash\nprintf "%s\\n" "$@" > "$GH_ARGS"\n', { mode: 0o755 });
	for (const result of ["success", "failure", "cancelled", "skipped"]) {
		const args = join(stub, "args");
		rmSync(args, { force: true });
		const run = Bun.spawnSync(["bash", "--noprofile", "--norc", "-eo", "pipefail", "-c", report.steps[0].run], {
			env: {
				PATH: `${stub}:${process.env.PATH}`,
				GH_ARGS: args,
				REPO: "o/r",
				SHA: "a".repeat(40),
				RESULT: result,
			},
		});
		assert.equal(run.exitCode === 0, result === "success", `${result} exit`);
		const posted = readFileSync(args, "utf8").split("\n");
		assert.ok(
			posted.includes(`conclusion=${result === "success" ? "success" : "failure"}`),
			`${result} conclusion`,
		);
	}
} finally {
	rmSync(stub, { recursive: true, force: true });
}

// A cancel or timeout signals only the step's process; exec makes that just, so
// the dispatcher hears it and prints what a hung lane waits on.
const gates = [build.jobs.build.steps, nightly.jobs.gate.steps]
	.flat()
	.filter((step: { run?: string }) => /\bjust (ci|gate)\b(?!-)/.test(step.run ?? ""));
assert.equal(gates.length, 2);
for (const step of gates) assert.match(step.run, /^exec nix develop --command just (ci|gate)\b/m, step.name);

// Every other workflow stays out of pull requests and main's merge groups:
// agents run checks locally, and optional checks run nightly on main.
const files = (await readdir(".github/workflows")).filter((file) => file.endsWith(".yml"));
for (const file of files) {
	const name = file.replace(/\.yml$/, "");
	if (["queue", "queue-pr"].includes(name)) continue;
	const config = await workflow(name);
	assert.equal(config.on.merge_group, undefined, `${file} must not run in the merge queue`);
	assert.equal(config.on.pull_request, undefined, `${file} must not check pull requests`);
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
assert.ok(enqueue.run.includes('quest merge "$PR_URL" --land "$PR_HEAD"'));
// Requesting a landing only enqueues the pinned head; the queue and its watcher do the rest.
assert.ok(!enqueue.run.includes("quest land"), "Dependabot must not run the land daemons' commands");
assert.ok(!JSON.stringify(request).includes("gh pr merge"));

// Platform runs its native suites over every package, and Nightly runs it on main.
assert.deepEqual(Object.keys(platform.jobs).sort(), ["macos", "scope", "windows"]);
// The selector runs before any toolchain, through the same query as the merge queue.
assert.equal(platform.jobs.scope.steps.at(-1).run, 'just ci-select-platform --all >> "$GITHUB_OUTPUT"');
for (const job of [platform.jobs.macos, platform.jobs.windows]) {
	assert.equal(condition(job.if, "schedule", false), false);
	assert.equal(condition(job.if, "schedule", true), true);
}
assert.equal(nightly.jobs.platform.uses, "./.github/workflows/platform.yml");
assert.deepEqual(platform.on.push.branches, ["release"]);

// Nightly covers what the merge queue skips.
for (const name of ["x11", "android", "cpp", "audio-quality", "oss-scanner", "runner-vm", "features"]) {
	assert.ok(
		Object.values(nightly.jobs).some(
			(job) => (job as { uses?: string }).uses === `./.github/workflows/${name}.yml`,
		),
		`nightly must call ${name}.yml`,
	);
}
for (const job of ["release-swift-lib", "release-shared-ffi", "release-dart", "release-js", "release-kt-lib"]) {
	assert.ok(nightly.jobs[job], `nightly must dry-run ${job}`);
}

assert.deepEqual((await workflow("obs")).on.push.branches, ["release"]);

// Release Dart FFI only runs nightly and on tags, so it packages every platform.
const targets = JSON.parse(dart.jobs.plan.steps[0].env.ALL);
assert.ok(targets.some((target: string) => target.includes("-apple-")));
assert.ok(targets.some((target: string) => target.includes("-windows-")));

// The feature matrix shares none of the queue's default-feature test artifacts, so
// it runs nightly.
const features = await workflow("features");
assert.deepEqual(features.jobs.features.strategy.matrix.recipe, ["rs features"]);
// Nightly runs the whole gate over every package on a GitHub-hosted runner.
// The exact command: `just gate -rs` would skip a module.
assert.match(nightly.jobs.gate.steps.at(-1).run, /^exec nix develop --command just gate$/m);
// Public async work runs on GitHub's runners; only the merge queue is self-hosted.
for (const [id, job] of Object.entries(nightly.jobs)) {
	const runner = JSON.stringify((job as { "runs-on"?: unknown })["runs-on"] ?? "");
	assert.ok(!runner.includes("self-hosted"), `nightly ${id} must run on a GitHub-hosted runner`);
}
console.log("queue event policy passed");
