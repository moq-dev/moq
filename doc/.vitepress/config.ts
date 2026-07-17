import { defineConfig } from "vitepress";

export default defineConfig({
	title: "Media over QUIC",
	description: "Real-time latency at massive scale",
	base: "/",

	head: [
		["link", { rel: "icon", href: "/favicon.svg", type: "image/svg+xml" }],
		["meta", { property: "og:type", content: "website" }],
		["meta", { property: "og:title", content: "Media over QUIC" }],
		[
			"meta",
			{
				property: "og:description",
				content: "Real-time latency at massive scale",
			},
		],
		["meta", { property: "og:image", content: "https://doc.moq.dev/icon.png" }],
		["meta", { property: "og:image:width", content: "163" }],
		["meta", { property: "og:image:height", content: "150" }],
		["meta", { property: "og:url", content: "https://doc.moq.dev" }],
		["meta", { property: "og:site_name", content: "Media over QUIC" }],
		["meta", { name: "twitter:card", content: "summary_large_image" }],
		["meta", { name: "twitter:title", content: "Media over QUIC" }],
		[
			"meta",
			{
				name: "twitter:description",
				content: "Real-time latency at massive scale",
			},
		],
		["meta", { name: "twitter:image", content: "https://doc.moq.dev/icon.png" }],
		["meta", { name: "theme-color", content: "#0f172a" }],
	],

	appearance: "force-dark",

	themeConfig: {
		logo: "/favicon.svg",

		nav: [
			{ text: "Setup", link: "/setup/" },
			{ text: "Concepts", link: "/concept/" },
			{ text: "Apps", link: "/bin/" },
			{
				text: "Libraries",
				link: "/lib/",
				items: [
					{ text: "Rust", link: "/lib/rs/" },
					{ text: "TypeScript", link: "/lib/js/" },
					{ text: "C", link: "/lib/c/" },
					{ text: "Python", link: "/lib/py/" },
					{ text: "Kotlin", link: "/lib/kt/" },
					{ text: "Swift", link: "/lib/swift/" },
					{ text: "Go", link: "/lib/go/" },
				],
			},
		],

		sidebar: {
			"/setup/": [
				{
					text: "Setup",
					link: "/setup/",
					items: [
						{ text: "Development", link: "/setup/dev" },
						{ text: "Production", link: "/setup/prod" },
						{ text: "Linux Packages", link: "/setup/linux" },
						{ text: "Windows", link: "/setup/windows" },
					],
				},
				{
					text: "Demos",
					items: [
						{ text: "Web", link: "/setup/demo/web" },
						{ text: "MoQ Boy", link: "/setup/demo/boy" },
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
								{ text: "web-socket", link: "/concept/layer/web-socket" },
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
								{ text: "Interop", link: "/concept/standard/interop" },
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

			"/bin/": [
				{
					text: "Applications",
					link: "/bin/",
					items: [
						{
							text: "Relay",
							link: "/bin/relay/",
							items: [
								{ text: "Configuration", link: "/bin/relay/config" },
								{ text: "Authentication", link: "/bin/relay/auth" },
								{ text: "Clustering", link: "/bin/relay/cluster" },
								{ text: "HTTP", link: "/bin/relay/http" },
								{ text: "Production", link: "/bin/relay/prod" },
							],
						},
						{ text: "CLI", link: "/bin/cli" },
						{ text: "WebRTC", link: "/bin/rtc" },
						{ text: "RTMP", link: "/bin/rtmp" },
						{ text: "OBS", link: "/bin/obs" },
						{ text: "GStreamer", link: "/bin/gstreamer" },
						{ text: "HLS", link: "/bin/hls" },
						{ text: "Web", link: "/bin/web" },
					],
				},
			],

			"/lib/": [
				{
					text: "Libraries",
					link: "/lib/",
					items: [
						{
							text: "Rust",
							link: "/lib/rs/",
							items: [
								{
									text: "Environments",
									link: "/lib/rs/env/",
									items: [
										{ text: "Native", link: "/lib/rs/env/native" },
										{ text: "WASM", link: "/lib/rs/env/wasm" },
									],
								},
								{
									text: "Crates",
									link: "/lib/rs/crate",
									items: [
										{ text: "moq-net", link: "/lib/rs/crate/moq-net" },
										{ text: "moq-native", link: "/lib/rs/crate/moq-native" },
										{ text: "moq-token", link: "/lib/rs/crate/moq-token" },
										{ text: "hang", link: "/lib/rs/crate/hang" },
										{ text: "web-transport", link: "/lib/rs/crate/web-transport" },
										{ text: "libmoq", link: "/lib/rs/crate/libmoq" },
									],
								},
							],
						},
						{
							text: "TypeScript",
							link: "/lib/js/",
							items: [
								{
									text: "Environments",
									link: "/lib/js/env/",
									items: [
										{ text: "Web", link: "/lib/js/env/web" },
										{ text: "Native", link: "/lib/js/env/native" },
									],
								},
								{
									text: "Packages",
									link: "/lib/js/@moq",
									items: [
										{ text: "@moq/net", link: "/lib/js/@moq/net" },
										{ text: "@moq/hang", link: "/lib/js/@moq/hang" },
										{ text: "@moq/watch", link: "/lib/js/@moq/watch" },
										{ text: "@moq/publish", link: "/lib/js/@moq/publish" },
										{ text: "@moq/token", link: "/lib/js/@moq/token" },
										{ text: "@moq/signals", link: "/lib/js/@moq/signals" },
									],
								},
							],
						},
						{
							text: "C",
							link: "/lib/c/",
							items: [{ text: "libmoq", link: "/lib/rs/crate/libmoq" }],
						},
						{
							text: "Python",
							link: "/lib/py/",
							items: [{ text: "moq-rs", link: "/lib/py/moq-rs" }],
						},
						{
							text: "Kotlin",
							link: "/lib/kt/",
							items: [{ text: "dev.moq:moq", link: "/lib/kt/moq" }],
						},
						{
							text: "Swift",
							link: "/lib/swift/",
							items: [{ text: "Moq", link: "/lib/swift/moq" }],
						},
						{
							text: "Go",
							link: "/lib/go/",
							items: [
								{ text: "moq", link: "/lib/go/moq" },
								{ text: "moq-ffi", link: "/lib/go/moq-ffi" },
							],
						},
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
		// Localhost URLs are intentional for development examples and aren't
		// reachable at build time (e.g. the relay on :4443, the dev server on :5173).
		/^https?:\/\/localhost(:\d+)?(\/|\?|#|$)/,
	],
});
