/**
 * Browser entry point: register the `@moq/publish` + `@moq/watch` custom elements
 * from the workspace, then run the shared role logic.
 *
 * @module
 */
import "@moq/publish/element";
import "@moq/watch/element";
import "@moq/watch/ui";
import "./setup.ts";
