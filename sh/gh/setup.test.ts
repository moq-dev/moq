// The hosted runner still owns timing and action execution. This checks its setup gates.
import { expect, test } from "bun:test";

type Step = {
	name?: string;
	id?: string;
	uses?: string;
	run?: string;
	if?: string;
	env?: Record<string, string>;
	"continue-on-error"?: boolean;
};
type Job = { name: string; steps: Step[] };
const workflow = Bun.YAML.parse(await Bun.file(".github/workflows/check.yml").text()) as {
	on: Record<string, unknown>;
	jobs: Record<string, Job>;
};

for (const name of ["check", "test"]) {
	test(`${name} queries before setup and keeps its required check name`, () => {
		const job = workflow.jobs[name];
		expect(job.name).toBe(name === "check" ? "Check" : "Test");
		const scopeIndex = job.steps.findIndex((step) => step.id === "scope");
		expect(scopeIndex).toBeGreaterThan(0);
		const scope = job.steps[scopeIndex];
		expect(scope.run).toBe(`just ci-select ${name} "$CI_BASE" >> "$GITHUB_OUTPUT"`);
		// biome-ignore lint/suspicious/noTemplateCurlyInString: GitHub expression is literal YAML.
		expect(scope.env?.CI_BASE).toBe("${{ github.event.merge_group.base_sha }}");
		expect(scope.if).toBeUndefined();
		expect(scope["continue-on-error"]).toBeUndefined();
		expect(job.steps.slice(0, scopeIndex).some((step) => step.uses === "./.github/actions/just")).toBe(true);

		// No expensive action or command may bypass the query, including future additions.
		// The Rust cache is the largest restore, so it also waits for selected work that compiles Rust.
		const setup = job.steps.slice(scopeIndex + 1);
		expect(setup.some((step) => step.uses?.startsWith("DeterminateSystems/nix-installer-action@"))).toBe(true);
		expect(setup.some((step) => step.uses === "./.github/actions/rust-cache")).toBe(true);
		expect(setup.at(-1)?.name).toBe(job.name);
		for (const step of setup) {
			const output = step.uses === "./.github/actions/rust-cache" ? "rust" : "needed";
			expect(step.if).toBe(`steps.scope.outputs.${output} == 'true'`);
		}
	});
}

test("scope queries support both merge groups and release PRs without path filters", () => {
	expect(workflow.on.merge_group).toEqual({ types: ["checks_requested"] });
	expect(workflow.on.pull_request).toBeDefined();
	expect(workflow.on.pull_request).not.toHaveProperty("paths");
});
