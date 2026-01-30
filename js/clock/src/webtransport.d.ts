// Type declarations for @fails-components/webtransport
// This package provides WebTransport polyfill for Node.js/Bun environments.

declare module "@fails-components/webtransport" {
	export const WebTransport: typeof globalThis.WebTransport;
	export const quicheLoaded: Promise<void>;
}
