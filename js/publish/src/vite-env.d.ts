/// <reference types="vite/client" />

// Add support for worklet imports
declare module "*?worker&url" {
	const workerUrl: string;
	export default workerUrl;
}
