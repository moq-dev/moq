// biome-ignore-all lint/suspicious/noTemplateCurlyInString: GitHub Actions expressions are literal strings.
// Runner timing and action execution stay in CI. These tests check the cache and corpus setup.
import { expect, test } from "bun:test";

type Step = {
	name?: string;
	id?: string;
	uses?: string;
	run?: string;
	if?: string;
};

test("self-hosted queue jobs use local caches without R2 credentials", async () => {
	const action = Bun.YAML.parse(await Bun.file(".github/actions/rust-cache/action.yml").text());
	const setup = action.runs.steps.find((step: Step) => step.uses?.startsWith("jdx/mr-boxington-action@"));
	const expression = setup.with.backend
		.replaceAll("inputs.access-key-id", 'inputs["access-key-id"]')
		.replaceAll("inputs.secret-access-key", 'inputs["secret-access-key"]')
		.replace(/^\$\{\{\s*|\s*\}\}$/g, "");
	const backend = new Function("runner", "inputs", `return (${expression});`);
	// GitHub's expression `startsWith`, so the guard evaluates as written.
	const startsWith = (value: string, prefix: string) => value.startsWith(prefix);
	const guard = new Function("runner", "github", "startsWith", `return (${action.runs.steps[0].if});`);
	const queue = "refs/heads/gh-readonly-queue/main/pr-1-0123456789abcdef0123456789abcdef01234567";
	const refs = [
		"refs/heads/main",
		"refs/heads/feature",
		"refs/pull/1/merge",
		queue,
		"refs/heads/gh-readonly-queue/release/pr-1-0",
	];
	for (const name of ["Queue", "Land", "Check", "Untrusted"]) {
		for (const event of ["merge_group", "pull_request", "push"]) {
			for (const ref of refs) {
				const github = { workflow: name, event_name: event, ref };
				const trusted = name === "Queue" && event === "merge_group" && ref === queue;
				expect(guard({ environment: "self-hosted" }, github, startsWith)).toBe(!trusted);
				expect(guard({ environment: "github-hosted" }, github, startsWith)).toBe(false);
			}
		}
	}
	for (const credentials of [
		{ "access-key-id": "key", "secret-access-key": "secret" },
		{ "access-key-id": "", "secret-access-key": "" },
	]) {
		expect(backend({ environment: "self-hosted" }, credentials)).toBe("local");
		expect(backend({ environment: "github-hosted" }, credentials)).toBe(
			credentials["access-key-id"] ? "remote" : "local",
		);
	}
	for (const name of ["Cache credentials", "Check the cache"]) {
		expect(action.runs.steps.find((step: Step) => step.name === name).if).toStartWith(
			"runner.environment != 'self-hosted'",
		);
	}
	expect(action.runs.steps[0].run).toContain("exit 1");
});

test("nightly fuzzing restores and saves minimized target-specific corpora", async () => {
	const nightly = Bun.YAML.parse(await Bun.file(".github/workflows/nightly.yml").text());
	const steps = nightly.jobs.fuzz.steps;
	const restore = steps.findIndex((step: Step) => step.uses?.startsWith("actions/cache/restore@"));
	const run = steps.findIndex((step: Step) => step.name === "Fuzz");
	const minimize = steps.findIndex((step: Step) => step.name === "Minimize corpus");
	const save = steps.findIndex((step: Step) => step.uses?.startsWith("actions/cache/save@"));
	expect(restore).toBeGreaterThan(0);
	expect(run).toBeGreaterThan(restore);
	expect(minimize).toBeGreaterThan(run);
	expect(save).toBeGreaterThan(minimize);
	const cached = steps[restore].with;
	expect(cached.path).toBe("rs/*/fuzz/corpus/${{ matrix.target }}");
	expect(cached.key).toContain("${{ matrix.target }}");
	expect(cached.key).toContain("${{ github.run_id }}-${{ github.run_attempt }}");
	expect(cached.key).toStartWith(cached["restore-keys"].trim());
	expect(steps[save].with.path).toBe(cached.path);
	expect(steps[save].with.key).toBe(cached.key);
	// Default success guards preserve the last healthy corpus after a crash or failed minimization.
	expect(steps[minimize].if).toBeUndefined();
	expect(steps[save].if).toBeUndefined();
	expect(steps[minimize].run).toBe("nix develop --command just rs fuzz --minimize ${{ matrix.target }}");
	expect(steps.find((step: Step) => step.name === "Preserve failure inputs").if).toBe("${{ failure() }}");
});
