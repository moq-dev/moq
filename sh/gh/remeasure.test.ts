// biome-ignore-all lint/suspicious/noTemplateCurlyInString: GitHub Actions expressions are literal strings.
import { expect, test } from "bun:test";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { relative, resolve } from "node:path";
import { changedPins, keepEvidence } from "./remeasure";

test("only packages pinned by a break trigger measurement", () => {
	const breaks = { draft: { releases: { cli: "1", relay: "2" } } };
	expect(changedPins(breaks, { cli: "1", relay: "2", unrelated: "3" })).toBe(false);
	expect(changedPins(breaks, { cli: "2", relay: "2" })).toBe(true);
	expect(changedPins(breaks, { cli: "1", relay: "3" })).toBe(true);
	expect(changedPins({}, { cli: "2" })).toBe(false);
	expect(() => changedPins(breaks, { cli: "1" })).toThrow("Unresolved");
});

test("kept evidence is the logs and the matrix inputs, not build output", () => {
	mkdirSync(".tmp", { recursive: true });
	const directory = mkdtempSync(resolve(".tmp/remeasure-test-"));
	const runs = `${directory}/runs`;
	const kept = [
		"wire-compat-a/versions.json",
		"wire-compat-a/shared",
		"wire-compat-a/controls",
		"wire-compat-a/measured",
		"wire-compat-a/current-help",
		"wire-compat-a/released-help",
		"wire-compat-a/resolved-js.lock",
		"interop-b/relay.log",
		"interop-b/moq-lite-03/pub-rust.log",
		"interop-b/fetch-a.track",
		"interop-b/fetch-a.0.json",
		"interop-b/fetch-a.1.json",
		"interop-b/data-a.fetch",
		"interop-b/rust-rust-audio.stats",
	];
	const dropped = [
		"wire-compat-a/current/moq",
		"wire-compat-a/released/bin/moq-relay",
		"wire-compat-a/js/node_modules/pkg/install.log",
		"wire-compat-a/key.jwk",
		"interop-b/bin/moq",
		"interop-b/tone-avc.mov",
		"interop-b/rust-rust-audio.wav",
		"interop-b/c-build.json",
		"interop-b/moq-lite-03/versions.json",
	];
	try {
		for (const path of [...kept, ...dropped]) {
			mkdirSync(resolve(runs, path, ".."), { recursive: true });
			writeFileSync(`${runs}/${path}`, path);
		}
		keepEvidence(runs, `${directory}/logs`);
		const copied = readdirSync(`${directory}/logs`, { recursive: true, withFileTypes: true })
			.filter((entry) => entry.isFile())
			.map((entry) => relative(`${directory}/logs`, `${entry.parentPath}/${entry.name}`));
		expect(copied.sort()).toEqual(kept.sort());
		expect(readFileSync(`${directory}/logs/interop-b/relay.log`, "utf8")).toBe("interop-b/relay.log");
		// Nothing ran: nothing to keep, and no error to mask the measurement's own.
		keepEvidence(`${directory}/missing`, `${directory}/logs`);
	} finally {
		rmSync(directory, { recursive: true, force: true });
	}
});

test("release remeasurement executes trusted code with protected write credentials", async () => {
	const workflow = Bun.YAML.parse(await Bun.file(".github/workflows/release-remeasure.yml").text());
	expect(workflow.on.workflow_run).toEqual({
		workflows: ["Release RS", "Release JS"],
		types: ["completed"],
		branches: ["release"],
	});
	expect(workflow.permissions).toEqual({ contents: "read" });
	const job = workflow.jobs.remeasure;
	expect(job.if).toContain("github.event.workflow_run.head_repository.full_name == github.repository");
	expect(job.environment).toBe("bot");
	expect(job.steps[0].with).toEqual({ "persist-credentials": false });
	const token = job.steps.find((step: { id?: string }) => step.id === "token");
	expect(token.if).toContain("github.ref == 'refs/heads/main'");
	expect(token.if).toContain("!inputs.dry-run");
});

test("publishing attempts remeasure even after a partial publish fails", async () => {
	const workflow = Bun.YAML.parse(await Bun.file(".github/workflows/release-remeasure.yml").text());
	const evaluate = new Function("github", "contains", "fromJSON", `return (${workflow.jobs.remeasure.if});`);
	for (const event of ["push", "workflow_dispatch", "workflow_call", "pull_request"]) {
		for (const conclusion of ["success", "failure", "cancelled"]) {
			for (const repository of ["moq-dev/moq", "outsider/moq"]) {
				const github = {
					repository: "moq-dev/moq",
					ref: "refs/heads/main",
					event_name: "workflow_run",
					event: {
						workflow_run: {
							event,
							conclusion,
							head_repository: { full_name: repository },
						},
					},
				};
				expect(evaluate(github, (list: string[], value: string) => list.includes(value), JSON.parse)).toBe(
					repository === "moq-dev/moq" && ["push", "workflow_dispatch"].includes(event),
				);
			}
		}
	}
});

test("later publications update one PR and request landing on its new head", async () => {
	const workflow = Bun.YAML.parse(await Bun.file(".github/workflows/release-remeasure.yml").text());
	const propose = workflow.jobs.remeasure.steps.find((step: { id?: string }) => step.id === "pr");
	expect(propose.with.branch).toBe("release-remeasure");
	// Git cannot hold a branch under another branch's name as a directory.
	for (const branch of [propose.with.base, ...workflow.on.workflow_run.branches]) {
		expect(propose.with.branch.startsWith(`${branch}/`)).toBe(false);
	}
	expect(propose.with["body-path"]).toBe(".tmp/remeasure/body.md");
	const landing = workflow.jobs.remeasure.steps.at(-1);
	expect(landing.run).toContain('quest merge "$PR_URL" --land "$PR_HEAD"');
	expect(landing.env.PR_HEAD).toBe("${{ steps.pr.outputs.pull-request-head-sha }}");
});

test("generated PR preserves the resolver report and credits the workflow", () => {
	mkdirSync(".tmp", { recursive: true });
	const directory = mkdtempSync(resolve(".tmp/remeasure-test-"));
	const script = resolve("sh/gh/remeasure.ts");
	const report =
		"draft: still fails in 1 of 2 cells; re-pinned\n  session/lite-07/current/released\nold: no longer fails in 3 cells; deleted\n";
	try {
		mkdirSync(`${directory}/test/interop/compat`, { recursive: true });
		mkdirSync(`${directory}/bin`);
		writeFileSync(
			`${directory}/test/interop/compat/planned-breaks.json`,
			JSON.stringify({ draft: { releases: { cli: "1" } } }),
		);
		writeFileSync(`${directory}/bin/bun`, `#!/bin/sh\nprintf '{"cli":"2"}\\n' > "$3"\n`, { mode: 0o755 });
		writeFileSync(
			`${directory}/bin/just`,
			`#!/bin/sh
cat > "$REMEASURE_REPORT" <<'REPORT'
${report}REPORT
printf '{}\\n' > test/interop/compat/planned-breaks.json
`,
			{ mode: 0o755 },
		);
		const result = Bun.spawnSync([process.execPath, script], {
			cwd: directory,
			env: {
				...process.env,
				PATH: `${directory}/bin:${process.env.PATH}`,
				GITHUB_ACTIONS: "true",
				GITHUB_SERVER_URL: "https://github.com",
				GITHUB_REPOSITORY: "moq-dev/moq",
				GITHUB_RUN_ID: "123",
				GITHUB_OUTPUT: `${directory}/output`,
				REMEASURE_FORCE: "",
			},
		});
		expect(result.exitCode).toBe(0);
		const body = readFileSync(`${directory}/.tmp/remeasure/body.md`, "utf8");
		expect(body).toContain(report.trimEnd());
		expect(body).toContain("https://github.com/moq-dev/moq/actions/runs/123");
		expect(body).toContain("Generated by the Release remeasure workflow.");
		expect(body).not.toContain("Written by");
		expect(readFileSync(`${directory}/output`, "utf8")).toBe("changed=true\n");
	} finally {
		rmSync(directory, { recursive: true, force: true });
	}
});

test("a failed measurement still uploads its logs and none of its binaries", async () => {
	const workflow = Bun.YAML.parse(await Bun.file(".github/workflows/release-remeasure.yml").text());
	const upload = workflow.jobs.remeasure.steps.find((step: { name?: string }) => step.name === "Upload measurements");
	expect(upload.with.path).toBe(".tmp/remeasure");
	expect(upload.if).toBe("${{ !cancelled() }}");

	mkdirSync(".tmp", { recursive: true });
	const directory = mkdtempSync(resolve(".tmp/remeasure-test-"));
	try {
		mkdirSync(`${directory}/test/interop/compat`, { recursive: true });
		mkdirSync(`${directory}/bin`);
		writeFileSync(`${directory}/test/interop/compat/planned-breaks.json`, "{}");
		writeFileSync(`${directory}/bin/bun`, `#!/bin/sh\nprintf '{}\\n' > "$3"\n`, { mode: 0o755 });
		writeFileSync(
			`${directory}/bin/just`,
			`#!/bin/sh
mkdir -p "$HARNESS_RUNS/interop-x/bin"
echo failed > "$HARNESS_RUNS/interop-x/sub.log"
echo binary > "$HARNESS_RUNS/interop-x/bin/moq"
exit 1
`,
			{ mode: 0o755 },
		);
		const result = Bun.spawnSync([process.execPath, resolve("sh/gh/remeasure.ts")], {
			cwd: directory,
			env: { ...process.env, PATH: `${directory}/bin:${process.env.PATH}`, REMEASURE_FORCE: "1" },
		});
		expect(result.exitCode).not.toBe(0);
		expect(readFileSync(`${directory}/.tmp/remeasure/logs/interop-x/sub.log`, "utf8")).toBe("failed\n");
		expect(existsSync(`${directory}/.tmp/remeasure/logs/interop-x/bin`)).toBe(false);
		expect(existsSync(`${directory}/.tmp/remeasure-runs`)).toBe(false);
	} finally {
		rmSync(directory, { recursive: true, force: true });
	}
});
