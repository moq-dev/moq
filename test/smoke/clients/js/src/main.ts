// Bundler entry point for the vite and esbuild variants. Registers the public
// custom elements from the installed npm packages, then runs the shared role
// logic. The jsdelivr variant does the same via CDN imports in its index.html.
import "@moq/publish/element";
import "@moq/watch/element";
import "../jsdelivr/setup.js";
