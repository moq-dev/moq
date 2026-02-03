/// <reference types="vite/client" />

// Add support for SVG imports as raw strings
declare module "*.svg?raw" {
	const content: string;
	export default content;
}

// Add support for CSS imports as inline strings
declare module "*?inline" {
	const content: string;
	export default content;
}

// Add support for worklet imports
declare module "*?worker&url" {
	const workerUrl: string;
	export default workerUrl;
}
