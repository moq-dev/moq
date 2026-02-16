import { defineConfig } from "vitepress";

export default defineConfig({
	title: "Media over QUIC",
	description: "Real-time latency at massive scale",
	base: "/",

	head: [["link", { rel: "icon", href: "/icon.svg", type: "image/svg+xml" }]],

	appearance: "force-dark",

	themeConfig: {
		logo: "/icon.svg",

		nav: [
			{ text: "Setup", link: "/setup/" },
			{ text: "Concepts", link: "/concept/" },
			{ text: "Apps", link: "/app/" },
			{ text: "Rust", link: "/rs/" },
			{ text: "TypeScript", link: "/js/" },
		],

		sidebar: {
			"/setup/": [
				{
					text: "Setup",
					link: "/setup/",
					items: [
						{ text: "Development", link: "/setup/dev" },
						{ text: "Production", link: "/setup/prod" },
					],
				},
			],

			"/concept/": [
				{
					text: "Concepts",
					link: "/concept/",
					items: [
						{
							text: "Layers",
							link: "/concept/layer/",
							items: [
								{ text: "quic", link: "/concept/layer/quic" },
								{ text: "web-transport", link: "/concept/layer/web-transport" },
								{ text: "moq-lite", link: "/concept/layer/moq-lite" },
								{ text: "hang", link: "/concept/layer/hang" },
							],
						},
						{
							text: "Standards",
							link: "/concept/standard/",
							items: [
								{ text: "MoqTransport", link: "/concept/standard/moq-transport" },
								{ text: "MSF", link: "/concept/standard/msf" },
								{ text: "LOC", link: "/concept/standard/loc" },
							],
						},
						{
							text: "Use Cases",
							link: "/concept/use-case/",
							items: [
								{ text: "Contribution", link: "/concept/use-case/contribution" },
								{ text: "Distribution", link: "/concept/use-case/distribution" },
								{ text: "Conferencing", link: "/concept/use-case/conferencing" },
								{ text: "AI", link: "/concept/use-case/ai" },
								{ text: "Other", link: "/concept/use-case/other" },
							],
						},
					],
				},
			],

			"/app/": [
				{
					text: "Applications",
					link: "/app/",
					items: [
						{
							text: "Relay",
							link: "/app/relay/",
							items: [
								{ text: "Configuration", link: "/app/relay/config" },
								{ text: "Authentication", link: "/app/relay/auth" },
								{ text: "Clustering", link: "/app/relay/cluster" },
								{ text: "HTTP", link: "/app/relay/http" },
								{ text: "Production", link: "/app/relay/production" },
							],
						},
						{ text: "CLI", link: "/app/cli" },
						{ text: "OBS", link: "/app/plugin/obs" },
						{ text: "Gstreamer", link: "/app/plugin/gstreamer" },
						{ text: "Web", link: "/app/plugin/web" },
					],
				},
			],

			"/rs/": [
				{
					text: "Environments",
					link: "/rs/env/",
					items: [
						{ text: "Native", link: "/rs/env/native" },
						{ text: "WASM", link: "/rs/env/wasm" },
					],
				},
				{
					link: "/rs/crate",
					items: [
						{ text: "moq-lite", link: "/rs/crate/moq-lite" },
						{ text: "moq-native", link: "/rs/crate/moq-native" },
						{ text: "moq-token", link: "/rs/crate/moq-token" },
						{ text: "hang", link: "/rs/crate/hang" },
						{ text: "web-transport", link: "/rs/crate/web-transport" },
					],
				},
			],

			"/js/": [
				{
					text: "Environments",
					link: "/js/env/",
					items: [
						{ text: "Web", link: "/js/env/web" },
						{ text: "Native", link: "/js/env/native" },
					],
				},
				{
					text: "Packages",
					link: "/js/@moq",
					items: [
						{ text: "@moq/lite", link: "/js/@moq/lite" },
						{
							text: "@moq/hang",
							link: "/js/@moq/hang/",
							items: [
								{ text: "Watch", link: "/js/@moq/hang/watch" },
								{ text: "Publish", link: "/js/@moq/hang/publish" },
							],
						},
						{ text: "@moq/watch", link: "/js/@moq/watch" },
						{ text: "@moq/publish", link: "/js/@moq/publish" },
						{ text: "@moq/ui-core", link: "/js/@moq/ui-core" },
						{ text: "@moq/token", link: "/js/@moq/token" },
						{ text: "@moq/signals", link: "/js/@moq/signals" },
						{ text: "@moq/web-transport-ws", link: "/js/@moq/web-transport-ws" },
					],
				},
			],
		},

		socialLinks: [
			{ icon: "github", link: "https://github.com/moq-dev/moq" },
			{ icon: "discord", link: "https://discord.gg/FCYF3p99mr" },
		],

		editLink: {
			pattern: "https://github.com/moq-dev/moq/edit/main/doc/:path",
			text: "Edit this page on GitHub",
		},

		search: {
			provider: "local",
		},

		lastUpdated: {
			text: "Last updated",
		},

		footer: {
			message: "Licensed under MIT or Apache-2.0",
			copyright: "Copyright © 2026-present moq.dev",
		},
	},

	markdown: {
		theme: "github-dark",
		lineNumbers: true,
	},

	ignoreDeadLinks: [
		// Localhost URLs are intentional for development
		"http://localhost:5173",
	],
});
