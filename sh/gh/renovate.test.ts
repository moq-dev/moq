import { expect, test } from "bun:test";
import config from "../../.github/renovate.cjs";

const updater = Bun.YAML.parse(await Bun.file(".github/workflows/renovate.yml").text());
const landing = Bun.YAML.parse(await Bun.file(".github/workflows/dependabot.yml").text());

function condition(expression: string, github: unknown, vars = {}) {
	return new Function("github", "vars", `return (${expression})`)(github, vars);
}

test("only main can start the credentialed updater", () => {
	for (const repository of ["moq-dev/moq", "someone/fork"]) {
		for (const ref of ["refs/heads/main", "refs/heads/release", "refs/heads/renovate/cargo", "refs/tags/main"]) {
			expect(condition(updater.jobs.cargo.if, { repository, ref })).toBe(
				repository === "moq-dev/moq" && ref === "refs/heads/main",
			);
		}
	}
	expect(updater.jobs.cargo.environment).toBe("renovate");
	expect(updater.permissions).toEqual({ contents: "read" });
	expect(Object.keys(updater.on).sort()).toEqual(["schedule", "workflow_dispatch"]);
	expect(config.requireConfig).toBe("ignored");
	expect(config.enabledManagers).toEqual(["cargo"]);
});

test("the hook admits generation without compilation or shell commands", () => {
	const allowed = (command: string) =>
		config.allowedCommands.some((pattern: string) => new RegExp(pattern).test(command));
	expect(allowed("cargo hakari generate")).toBe(true);
	for (const command of [
		"cargo build",
		"cargo check",
		"cargo hakari generate --all",
		"cargo hakari generate; cargo build",
		"cargo hakari generate\ncargo build",
		"cargo hakari generate && echo secret",
	]) {
		expect(allowed(command)).toBe(false);
	}
	expect(config.allowScripts).toBe(false);
	expect(config.allowPlugins).toBe(false);
	expect(config.allowShellExecutorForPostUpgradeCommands).toBe(false);
	expect(config.automerge).toBe(false);
	expect(config.vulnerabilityAlerts.automerge).toBe(false);
});

test("a dependency bot must match both the configured identity and this repository", () => {
	for (const configured of ["", "our-renovate[bot]"]) {
		for (const login of ["dependabot[bot]", "our-renovate[bot]", "renovate[bot]", "someone"]) {
			for (const headRepo of ["moq-dev/moq", "someone/fork"]) {
				const github = {
					repository: "moq-dev/moq",
					event: { pull_request: { user: { login }, head: { repo: { full_name: headRepo } } } },
				};
				expect(condition(landing.jobs["request-landing"].if, github, { RENOVATE_BOT_LOGIN: configured })).toBe(
					headRepo === "moq-dev/moq" &&
						(login === "dependabot[bot]" || (configured !== "" && login === configured)),
				);
			}
		}
	}
	const steps = landing.jobs["request-landing"].steps;
	expect(steps.find((step: { name: string }) => step.name === "Request checked landing").env.PR_HEAD).toBe(
		`\${{ github.event.pull_request.head.sha }}`,
	);
	expect(steps.find((step: { name: string }) => step.name === "Request checked landing").run).toContain(
		'quest merge "$PR_URL" --land "$PR_HEAD"',
	);
});
