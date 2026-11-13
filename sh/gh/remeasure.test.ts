// biome-ignore-all lint/suspicious/noTemplateCurlyInString: GitHub Actions expressions are literal strings.
import { expect, test } from "bun:test";
import { changedPins } from "./remeasure";

test("only packages pinned by a break trigger measurement", () => {
	const breaks = { draft: { releases: { cli: "1", relay: "2" } } };
	expect(changedPins(breaks, { cli: "1", relay: "2", unrelated: "3" })).toBe(false);
	expect(changedPins(breaks, { cli: "2", relay: "2" })).toBe(true);
	expect(changedPins(breaks, { cli: "1", relay: "3" })).toBe(true);
	expect(changedPins({}, { cli: "2" })).toBe(false);
	expect(() => changedPins(breaks, { cli: "1" })).toThrow("Unresolved");
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
	expect(job.environment).toBe("${{ github.ref == 'refs/heads/main' && 'bot' || '' }}");
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
