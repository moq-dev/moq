module.exports = {
	// This is self-hosted configuration read from main, never from an update branch.
	platform: "github",
	repositories: ["moq-dev/moq"],
	onboarding: false,
	requireConfig: "ignored",
	dependencyDashboard: false,
	binarySource: "global",
	baseDir: ".tmp/renovate/work",
	cacheDir: ".tmp/renovate/cache",
	allowedCommands: ["^cargo hakari generate$"],
	allowScripts: false,
	allowPlugins: false,
	allowShellExecutorForPostUpgradeCommands: false,
	enabledManagers: ["cargo"],
	// Match the root workspace, not standalone smoke and benchmark workspaces.
	includePaths: ["Cargo.toml", "rs/**", "demo/carla/bridge/**"],
	// The clone retains this manifest for Cargo resolution; only extraction skips it.
	ignorePaths: ["rs/workspace-hack/**"],
	minimumReleaseAge: "7 days",
	minimumReleaseAgeBehaviour: "timestamp-required",
	internalChecksFilter: "strict",
	automerge: false,
	cargo: {
		postUpgradeTasks: {
			commands: ["cargo hakari generate"],
			// Hakari also refreshes the hack's edges in the root lockfile.
			fileFilters: ["rs/workspace-hack/Cargo.toml", "Cargo.lock"],
			executionMode: "branch",
		},
	},
	packageRules: [
		{
			matchManagers: ["cargo"],
			groupName: "cargo",
			separateMajorMinor: false,
		},
		{
			// The Go bindings come from a uniffi-bindgen-go build that only reads the
			// metadata of one uniffi minor (0.32 today). The generator is pinned in
			// flake.nix and five other places, so a uniffi minor moves with it by
			// hand; see the uniffi-bindgen-go comment in flake.nix.
			matchPackageNames: ["uniffi"],
			allowedVersions: "<0.33",
		},
		{
			// kio-macros is the only direct syn user, and it stays on syn 2 because
			// moq-net's graph already builds syn 2 (thiserror, tracing); syn 3 would
			// add a second syn build there and needs an API migration. Lift this
			// once those proc-macro crates move to syn 3; see rs/kio/macros/Cargo.toml.
			matchPackageNames: ["syn"],
			allowedVersions: "<3",
		},
	],
	// Security fixes bypass the release-age hold, as Dependabot's did.
	osvVulnerabilityAlerts: false,
	vulnerabilityAlerts: {
		enabled: true,
		minimumReleaseAge: "0 days",
		automerge: false,
	},
};
