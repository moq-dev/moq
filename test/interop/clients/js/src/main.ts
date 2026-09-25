/**
 * Browser entry point: count the platform resources the page holds, register the `@moq/publish` +
 * `@moq/watch` custom elements from the workspace, then run the shared role logic.
 *
 * The instrumentation import comes first: it wraps constructors, so anything built before it runs
 * would go uncounted.
 *
 * @module
 */
import "./instrument.ts";
import "@moq/publish/element";
import "@moq/watch/element";
import "@moq/watch/ui";
import "./setup.ts";
