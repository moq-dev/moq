// biome-ignore-all lint/suspicious/noTemplateCurlyInString: GitHub Actions expressions are literal strings.
import { expect, test } from "bun:test";
import { YAML } from "bun";

test("C++ dry runs check packaged archives and build OBS without publishing", async () => {
	const workflow = YAML.parse(await Bun.file(".github/workflows/release-cpp.yml").text());
	const jobs = workflow.jobs;
	expect(jobs.build.steps.find((step: { name: string }) => step.name === "Check packaged archive")?.run).toContain(
		"just cpp check --archive",
	);
	expect(jobs["obs-build"].needs).toEqual(["build", "release"]);
	const runs = new Function(
		"github",
		"needs",
		"always",
		"cancelled",
		`return (${jobs["obs-build"].if.replace(/\$\{\{|\}\}/g, "")});`,
	);
	for (const [ref, build, release, wanted] of [
		["branch", "success", "skipped", true],
		["tag", "success", "success", true],
		["tag", "success", "failure", false],
		["branch", "failure", "skipped", false],
	] as const) {
		expect(runs({ ref_type: ref }, { build: { result: build }, release: { result: release } }, () => true, () => false)).toBe(
			wanted,
		);
	}
	const steps = jobs["obs-build"].steps;
	expect(steps.find((step: { name: string }) => step.name === "Download local C++ archive").if).toBe(
		"github.ref_type != 'tag'",
	);
	expect(steps.find((step: { name: string }) => step.name === "Download local C++ archive").with.name).toBe(
		"moq-cpp-${{ matrix.target }}",
	);
	expect(steps.find((step: { name: string }) => step.name === "Build and package").run).toContain("--moq-archive");
	for (const name of ["release", "obs-release"]) expect(jobs[name].if).toBe("github.ref_type == 'tag'");
});
