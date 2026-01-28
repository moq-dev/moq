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
			{ text: "Dev", link: "/dev/" },
			{ text: "Relay", link: "/relay/" },
			{ text: "Rust", link: "/rust/" },
			{ text: "TypeScript", link: "/ts/" },
			{ text: "Concepts", link: "/concepts/" },
		],

		sidebar: {
			"/dev/": [
				{
					text: "Getting Started",
					items: [{ text: "Quick Start", link: "/dev/" }],
				},
			],

			"/concepts/": [
				{
					text: "Concepts",
					items: [
						{ text: "Layers", link: "/concepts/" },
						{ text: "Protocol", link: "/concepts/protocol" },
						{ text: "Authentication", link: "/concepts/authentication" },
						{ text: "Standards", link: "/concepts/standards" },
					],
				},
				{
					text: "Comparisons",
					items: [
						{ text: "vs RTMP/SRT", link: "/concepts/contribution" },
						{ text: "vs HLS/DASH", link: "/concepts/distribution" },
						{ text: "vs WebRTC", link: "/concepts/conferencing" },
					],
				},
			],

			"/relay/": [
				{
					text: "Relay Server",
					items: [
						{ text: "Overview", link: "/relay/" },
						{ text: "Authentication", link: "/relay/auth" },
						{ text: "Clustering", link: "/relay/cluster" },
						{ text: "Production", link: "/relay/production" },
					],
				},
			],

			"/web/": [
				{
					text: "Web",
					items: [{ text: "Overview", link: "/web/" }],
				},
			],

			"/obs/": [
				{
					text: "OBS",
					items: [{ text: "Overview", link: "/obs/" }],
				},
			],

			"/gstreamer/": [
				{
					text: "GStreamer",
					items: [{ text: "Overview", link: "/gstreamer/" }],
				},
			],

			"/ffmpeg/": [
				{
					text: "FFmpeg",
					items: [{ text: "Overview", link: "/ffmpeg/" }],
				},
			],

			"/rust/": [
				{
					text: "Rust Libraries",
					items: [
						{ text: "Overview", link: "/rust/" },
						{ text: "moq-lite", link: "/rust/lite" },
						{ text: "hang", link: "/rust/hang" },
						{ text: "moq-token", link: "/rust/token" },
						{ text: "web-transport", link: "/rust/web-transport" },
						{ text: "Examples", link: "/rust/examples" },
					],
				},
			],

			"/ts/": [
				{
					text: "TypeScript Libraries",
					items: [
						{ text: "Overview", link: "/ts/" },
						{ text: "@moq/lite", link: "/ts/lite" },
						{
							text: "@moq/hang",
							link: "/ts/hang/",
							items: [
								{ text: "Watch", link: "/ts/hang/watch" },
								{ text: "Publish", link: "/ts/hang/publish" },
							],
						},
						{ text: "@moq/hang-ui", link: "/ts/hang-ui" },
						{ text: "@moq/token", link: "/ts/token" },
						{ text: "@moq/signals", link: "/ts/signals" },
						{ text: "Examples", link: "/ts/examples" },
					],
				},
			],
		},

		socialLinks: [
			{ icon: "github", link: "https://github.com/moq-dev/moq" },
			{ icon: "discord", link: "https://discord.gg/FCYF3p99mr" },
		],

		editLink: {
			pattern: "https://github.com/moq-dev/moq/edit/main/docs/:path",
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
			copyright: "Copyright © 2025-present MoQ Contributors",
		},
	},

	markdown: {
		theme: "github-dark",
		lineNumbers: true,
	},
});
