import { defineConfig } from "vitepress";
import { syncDrafts } from "./drafts";

// Generated before VitePress enumerates routes, so the pages and this sidebar
// list stay in sync with the kramdown-rfc sources under drafts/.
const drafts = syncDrafts();

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
					{ text: "Swift", link: "/lib/swift/" },
					{ text: "Kotlin", link: "/lib/kt/" },
					{ text: "Python", link: "/lib/py/" },
					{ text: "Go", link: "/lib/go/" },
					{ text: "Dart", link: "/lib/dart/" },
					{ text: "C", link: "/lib/c/" },
				],
			},
			{ text: "Drafts", link: "/draft/" },
		],

		sidebar: {
			"/setup/": [
				{
					text: "Setup",
					link: "/setup/",
					items: [
						{ text: "Quick start", link: "/setup/" },
						{ text: "Install", link: "/setup/install" },
						{ text: "Development", link: "/setup/dev" },
						{ text: "Production", link: "/setup/prod" },
						{ text: "Coding agents", link: "/setup/agent" },
					],
				},
			],

			"/concept/": [
				{
					text: "Concepts",
					link: "/concept/",
					items: [
						{ text: "Overview", link: "/concept/" },
						{ text: "Transport", link: "/concept/transport" },
						{ text: "moq-lite", link: "/concept/moq-lite" },
						{ text: "hang", link: "/concept/hang" },
						{ text: "Standards", link: "/concept/standard" },
						{
							text: "Use cases",
							link: "/concept/use-case/",
							items: [
								{ text: "vs HLS/DASH", link: "/concept/use-case/distribution" },
								{ text: "vs RTMP/SRT", link: "/concept/use-case/contribution" },
								{ text: "vs WebRTC", link: "/concept/use-case/conferencing" },
								{ text: "For AI", link: "/concept/use-case/ai" },
							],
						},
					],
				},
			],

			"/draft/": [
				{
					text: "Internet-Drafts",
					link: "/draft/",
					items: drafts.map((draft) => ({ text: draft.title, link: draft.link })),
				},
			],

			"/bin/": [
				{
					text: "Applications",
					link: "/bin/",
					items: [
						{
							text: "moq-relay",
							link: "/bin/relay/",
							items: [
								{ text: "Configuration", link: "/bin/relay/config" },
								{ text: "Authentication", link: "/bin/relay/auth" },
								{ text: "Clustering", link: "/bin/relay/cluster" },
								{ text: "HTTP", link: "/bin/relay/http" },
								{ text: "Deployment", link: "/setup/prod" },
							],
						},
						{ text: "moq-cli", link: "/bin/cli" },
						{
							text: "Gateways",
							items: [
								{ text: "RTMP", link: "/bin/rtmp" },
								{ text: "SRT", link: "/bin/srt" },
								{ text: "WebRTC", link: "/bin/rtc" },
								{ text: "HLS", link: "/bin/hls" },
							],
						},
						{
							text: "Plugins",
							items: [
								{ text: "OBS Studio", link: "/bin/obs" },
								{ text: "GStreamer", link: "/bin/gstreamer" },
							],
						},
						{ text: "Demos", link: "/bin/demo" },
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
								{ text: "moq-net", link: "/lib/rs/moq-net" },
								{ text: "hang", link: "/lib/rs/hang" },
								{ text: "moq-mux", link: "/lib/rs/moq-mux" },
								{ text: "moq-video", link: "/lib/rs/moq-video" },
								{ text: "moq-audio", link: "/lib/rs/moq-audio" },
								{ text: "moq-token", link: "/lib/rs/moq-token" },
							],
						},
						{
							text: "TypeScript",
							link: "/lib/js/",
							items: [
								{ text: "@moq/net", link: "/lib/js/net" },
								{ text: "@moq/hang", link: "/lib/js/hang" },
								{ text: "@moq/watch", link: "/lib/js/watch" },
								{ text: "@moq/publish", link: "/lib/js/publish" },
								{ text: "@moq/token", link: "/lib/js/token" },
								{ text: "@moq/signals", link: "/lib/js/signals" },
							],
						},
						{ text: "Swift", link: "/lib/swift/" },
						{ text: "Kotlin", link: "/lib/kt/" },
						{ text: "Python", link: "/lib/py/" },
						{ text: "Go", link: "/lib/go/" },
						{ text: "Dart", link: "/lib/dart/" },
						{ text: "C", link: "/lib/c/" },
					],
				},
			],
		},

		socialLinks: [
			{ icon: "github", link: "https://github.com/moq-dev/moq" },
			{ icon: "discord", link: "https://discord.gg/FCYF3p99mr" },
		],

		editLink: {
			// /draft/ pages are generated, so edits go to the kramdown-rfc source
			// instead. VitePress ships this function to the browser by stringifying
			// it, so it has to stay closure-free: no imports, no outer variables.
			// The inverse of `slug()` in drafts.ts.
			pattern: ({ filePath }) => {
				const generated = filePath.startsWith("draft/") && filePath !== "draft/index.md";
				const slug = filePath.slice("draft/".length);
				const path = generated
					? `drafts/${slug.startsWith("draft-") ? slug : `draft-lcurley-${slug}`}`
					: `doc/${filePath}`;
				return `https://github.com/moq-dev/moq/edit/main/${path}`;
			},
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
