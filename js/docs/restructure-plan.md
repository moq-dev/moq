# Restructuring Plan: `@moq/hang` → Separate Watch/Publish Packages

## Background

The goal is to split watch and publish functionality out of `@moq/hang` so that `@moq/hang` is strictly the core protocol + container logic. Watch and publish move into their own packages (`@moq/watch`, `@moq/publish`), each co-locating their UI web component (e.g. `@moq/watch/ui`). Shared UI primitives (button, icons, stats, CSS variables) go into `@moq/ui-core`.

## Current Structure

| Package | Contents | Build |
|---|---|---|
| `@moq/hang` | catalog, container, watch, publish, support, util | `tsc` |
| `@moq/hang-ui` | watch UI, publish UI, shared UI (button, icons, stats, CSS) | vite lib mode |
| `@moq/hang-demo` | demo app consuming both | vite |

### Key dependencies

- `hang/src/watch/` and `hang/src/publish/` both import `../catalog` (broadcast, preview, user) and depend on `@moq/lite` + `@moq/signals`.
- `hang-ui/src/watch/` and `hang-ui/src/publish/` both import from `../shared/` (button, icons, stats, CSS variables).
- `hang-ui` is built with vite library mode and uses SolidJS web components.
- AudioWorklets exist in `publish/audio/capture-worklet.ts`.

## Target Structure

| Package | Contents | Build |
|---|---|---|
| `@moq/hang` | catalog, container, support, util only | tsc (or vite) |
| `@moq/watch` | watch logic + `@moq/watch/ui` web component | vite lib mode |
| `@moq/publish` | publish logic + `@moq/publish/ui` web component | vite lib mode |
| `@moq/ui-core` | shared UI: button, icons, stats, CSS variables | vite lib mode |
| `@moq/hang-demo` | updated demo | vite |

### Final dependency graph

```
@moq/lite  ←─────────────────────────────┐
@moq/signals ←───────────────────────┐    │
@moq/hang (catalog, container) ←──┐  │    │
@moq/ui-core (button, icons, etc) │  │    │
                                  │  │    │
@moq/watch     ───────────────────┴──┴────┤
  └─ /ui  (imports @moq/ui-core)          │
@moq/publish   ───────────────────┴──┴────┘
  └─ /ui  (imports @moq/ui-core)
```

---

## Cross-Cutting Concerns

These items span multiple milestones and should be addressed as each milestone is completed:

### Root `package.json` workspaces
The root `package.json` `workspaces` array currently lists `js/hang`, `js/hang-ui`, and `js/hang-demo`. As new packages are created and old ones removed:
- **M1:** Add `js/ui-core`
- **M2:** Add `js/watch`
- **M3:** Add `js/publish`
- **M6:** Remove `js/hang-ui`

### VitePress sidebar (`doc/.vitepress/config.ts`)
The sidebar currently nests Watch and Publish under `@moq/hang` and has a separate `@moq/hang-ui` entry. This needs restructuring:
- **M2–M3:** Move Watch/Publish to top-level sidebar entries (`@moq/watch`, `@moq/publish`)
- **M1:** Add `@moq/ui-core` sidebar entry
- **M6:** Remove `@moq/hang-ui` sidebar entry

### `CLAUDE.md`
References `hang-ui/` in the project structure and architecture layers. Update:
- **M1:** Add `ui-core/` to the project structure
- **M2–M3:** Add `watch/` and `publish/` to the project structure
- **M6:** Remove `hang-ui/` from the project structure, update architecture description

### Root `README.md`
Package table includes `@moq/hang-ui`. Update:
- **M1:** Add `@moq/ui-core` row
- **M2–M3:** Add `@moq/watch` and `@moq/publish` rows, update `@moq/hang` description
- **M6:** Remove `@moq/hang-ui` row

### `doc/index.md`
References `@moq/hang-ui` in the highlights list. Update alongside M6.

---

## Milestones

### Milestone 1: Create `@moq/ui-core` package

**Why first:** Prerequisite for Milestones 4 & 5. Extracting shared UI early establishes the `@moq/ui-core` contract before anything else moves.

**Scope:**
- Create `js/ui-core/` with `package.json` (`@moq/ui-core`), `vite.config.ts` (library mode), `tsconfig.json`.
- Move `hang-ui/src/shared/` contents into `ui-core/src/` (button, icons + SVGs, stats, `variables.css`, `flex.css`).
- The icon component uses `?raw` SVG imports — vite handles this natively.
- Update `hang-ui/src/watch/` and `hang-ui/src/publish/` to import from `@moq/ui-core` instead of `../../shared/`.
- Verify `hang-ui` still builds and `hang-demo` still works.

**README updates:**
- Create a new `js/ui-core/README.md` documenting the shared components (button, icon, stats), CSS variables, and usage.
- Move the stats component README (`hang-ui/src/shared/components/stats/README.md`) into `ui-core` alongside the component.
- Update `hang-ui/README.md` "shared/" section under "Project Structure" and "Module Overview" to note that shared components now come from `@moq/ui-core`.

**Other updates:**
- Add `js/ui-core` to root `package.json` workspaces.
- Add `@moq/ui-core` entry to `doc/.vitepress/config.ts` sidebar.
- Add `ui-core/` to `CLAUDE.md` project structure.
- Add `@moq/ui-core` row to root `README.md` package table.

**Exit criteria:** `@moq/ui-core` builds independently; `@moq/hang-ui` builds and works with `@moq/ui-core` as a dependency.

---

### Milestone 2: Create `@moq/watch` package (logic only)

**Scope:**
- Create `js/watch/` with `package.json`, `vite.config.ts`, `tsconfig.json`.
- Move `hang/src/watch/` → `watch/src/`.
- Change 3 internal imports (`../catalog` in `broadcast.ts`, `preview.ts`, `user.ts`) to `@moq/hang/catalog`.
- Dependencies: `@moq/hang` (for catalog), `@moq/lite`, `@moq/signals`, plus watch-specific deps from hang's `package.json` (e.g. `@kixelated/libavjs-webcodecs-polyfill`, `@libav.js/variant-opus-af`).
- Remove `./watch` and `./watch/element` exports from `hang/package.json`.
- Remove `export * as Watch from "./watch"` from `hang/src/index.ts`.

**README updates:**
- Create a new `js/watch/README.md` documenting the `@moq/watch` package: installation, JS API, and web component usage (`<hang-watch>` element and attributes).
- Pull the relevant content from `hang/README.md` sections: `<hang-watch>` attributes, the watch portion of the JS API example, and tree-shaking notes.
- Update `hang/README.md`:
  - Remove the `<hang-watch>` section and its code examples.
  - Remove watch-related JS API examples (`Hang.Watch.Broadcast`, etc.).
  - Add a note pointing users to `@moq/watch` for watch functionality.

**Doc site updates:**
- Move `doc/js/@moq/hang/watch.md` → `doc/js/@moq/watch.md` (or similar), update all import paths from `@moq/hang/watch` to `@moq/watch`.
- Update `doc/js/@moq/hang/index.md` to remove watch sections and link to the new `@moq/watch` page.
- Update `doc/js/index.md` "Quick Start" examples to use `@moq/watch` imports.
- Update `doc/js/env/web.md` `<hang-watch>` section: change imports from `@moq/hang/watch/element` to `@moq/watch/element`.
- Update `doc/js/@moq/signals.md` example import.
- Update `doc/.vitepress/config.ts` sidebar: move Watch out from under `@moq/hang` to a top-level `@moq/watch` entry.

**Other updates:**
- Add `js/watch` to root `package.json` workspaces.
- Add `watch/` to `CLAUDE.md` project structure.
- Add `@moq/watch` row to root `README.md` package table.

**Exit criteria:** `@moq/watch` builds. `@moq/hang` builds without watch.

> **Note:** Milestones 2 and 3 can be done in parallel.

---

### Milestone 3: Create `@moq/publish` package (logic only)

**Scope:**
- Create `js/publish/` with `package.json`, `vite.config.ts`, `tsconfig.json`.
- Move `hang/src/publish/` → `publish/src/`.
- Change 3 internal imports (`../catalog` in `broadcast.ts`, `preview.ts`, `user.ts`) to `@moq/hang/catalog`.
- **AudioWorklet handling:** `publish/audio/capture-worklet.ts` currently uses a vite-specific import. For now, inline it using the `toString()` pattern or a vite/rollup plugin. This is the trickiest part of this milestone.
- Dependencies: `@moq/hang`, `@moq/lite`, `@moq/signals`, `comlink`, `async-mutex`.
- Remove `./publish` and `./publish/element` exports from `hang/package.json`.
- Remove `export * as Publish from "./publish"` from `hang/src/index.ts`.

**README updates:**
- Create a new `js/publish/README.md` documenting the `@moq/publish` package: installation, JS API, and web component usage (`<hang-publish>` element and attributes).
- Pull the relevant content from `hang/README.md` sections: `<hang-publish>` attributes, the publish portion of the JS API example, and tree-shaking notes.
- Update `hang/README.md`:
  - Remove the `<hang-publish>` section and its code examples.
  - Remove publish-related JS API examples (`Hang.Publish.Broadcast`, etc.).
  - Add a note pointing users to `@moq/publish` for publish functionality.
  - After both M2 and M3, the "Web Components" and "Javascript API" sections should reference only `<hang-support>` or be replaced with a high-level overview pointing to the new packages.

**Doc site updates:**
- Move `doc/js/@moq/hang/publish.md` → `doc/js/@moq/publish.md` (or similar), update all import paths from `@moq/hang/publish` to `@moq/publish`.
- Update `doc/js/@moq/hang/index.md` to remove publish sections and link to the new `@moq/publish` page.
- Update `doc/js/index.md` "Quick Start" examples to use `@moq/publish` imports.
- Update `doc/js/env/web.md` `<hang-publish>` section: change imports from `@moq/hang/publish/element` to `@moq/publish/element`.
- Update `doc/.vitepress/config.ts` sidebar: move Publish out from under `@moq/hang` to a top-level `@moq/publish` entry.

**Other updates:**
- Add `js/publish` to root `package.json` workspaces.
- Add `publish/` to `CLAUDE.md` project structure.
- Add `@moq/publish` row to root `README.md` package table.

**Exit criteria:** `@moq/publish` builds with worklet inlined. `@moq/hang` builds without publish.

> **Note:** Milestones 2 and 3 can be done in parallel.

---

### Milestone 4: Move watch UI into `@moq/watch/ui`

**Scope:**
- Move `hang-ui/src/watch/` → `watch/src/ui/`.
- Add SolidJS + `solid-element` + `vite-plugin-solid` as dev deps of `@moq/watch`.
- Update vite config to have two entry points:
  - `watch/index` → `src/index.ts` (logic)
  - `watch/ui` → `src/ui/index.tsx` (web component)
- Update UI imports: `@moq/hang/watch/element` → local `../element`, shared components → `@moq/ui-core`.
- Add `@moq/ui-core` as a dependency.
- Configure `rollupOptions.external` to externalize `@moq/hang`, `@moq/lite`, `@moq/signals`, `@moq/ui-core`.
- Export `./ui` in `package.json`.

**README updates:**
- Update `js/watch/README.md` to add a "UI" section documenting the `@moq/watch/ui` entry point, how to use `<hang-watch-ui>`, and its dependency on `@moq/ui-core`.
- Update `hang-ui/README.md` to remove the `watch/` section from "Project Structure" and "Module Overview", noting it has moved to `@moq/watch/ui`.

**Doc site updates:**
- Update `doc/js/@moq/watch.md` SolidJS integration section: change `@moq/hang-ui/watch` to `@moq/watch/ui`.

**Exit criteria:** `@moq/watch` builds both logic and UI. The `hang-watch-ui` custom element registers and functions.

> **Note:** Milestones 4 and 5 can be done in parallel.

---

### Milestone 5: Move publish UI into `@moq/publish/ui`

**Scope:** Mirror of Milestone 4 for publish:
- Move `hang-ui/src/publish/` → `publish/src/ui/`.
- Same vite/SolidJS setup as watch.
- Two entry points: `@moq/publish` (logic) and `@moq/publish/ui` (web component).
- Update imports similarly.

**README updates:**
- Update `js/publish/README.md` to add a "UI" section documenting the `@moq/publish/ui` entry point, how to use `<hang-publish-ui>`, and its dependency on `@moq/ui-core`.
- Update `hang-ui/README.md` to remove the `publish/` section from "Project Structure" and "Module Overview", noting it has moved to `@moq/publish/ui`.

**Doc site updates:**
- Update `doc/js/@moq/publish.md` SolidJS integration section: change `@moq/hang-ui/publish` to `@moq/publish/ui`.

**Exit criteria:** `@moq/publish` builds both logic and UI. The `hang-publish-ui` custom element registers and functions.

> **Note:** Milestones 4 and 5 can be done in parallel.

---

### Milestone 6: Update `@moq/hang-demo` and remove `@moq/hang-ui`

**Scope:**
- Update `hang-demo/package.json`: replace `@moq/hang-ui` with `@moq/watch`, `@moq/publish` (and `@moq/ui-core` if used directly).
- Update all imports in `hang-demo`:
  - `@moq/hang-ui/watch` → `@moq/watch/ui`
  - `@moq/hang-ui/publish` → `@moq/publish/ui`
  - `@moq/hang/watch` → `@moq/watch`
  - `@moq/hang/publish` → `@moq/publish`
- Delete `js/hang-ui/` entirely.
- Full end-to-end test of the demo app.

**README updates:**
- Update `hang-demo/README.md` if it gains any references to specific packages (currently minimal).
- Delete `hang-ui/README.md` along with the `hang-ui` package.

**Doc site updates:**
- Delete `doc/js/@moq/hang-ui.md`.
- Remove `@moq/hang-ui` sidebar entry from `doc/.vitepress/config.ts`.
- Remove `js/hang-ui` from root `package.json` workspaces.
- Update `doc/index.md` to replace `@moq/hang-ui` reference with `@moq/watch/ui` and `@moq/publish/ui`.
- Update `doc/js/index.md` to remove `@moq/hang-ui` section and install command; replace with `@moq/watch`, `@moq/publish`, `@moq/ui-core`.
- Update `doc/js/env/web.md` SolidJS section to reference `@moq/watch/ui` and `@moq/publish/ui` instead of `@moq/hang-ui`.
- Remove `hang-ui/` from `CLAUDE.md` project structure, update architecture description.
- Remove `@moq/hang-ui` row from root `README.md` package table.

**Exit criteria:** `hang-demo` runs correctly with the new packages. `hang-ui` is gone.

---

### Milestone 7: Clean up `@moq/hang`

**Scope:**
- Delete `hang/src/watch/` and `hang/src/publish/` directories.
- Remove now-unused dependencies from `hang/package.json` (e.g. `@kixelated/libavjs-webcodecs-polyfill`, `@libav.js/variant-opus-af`, `comlink`, `async-mutex` — if only used by watch/publish).
- Remove `sideEffects` entries for watch/publish elements.
- Clean up `hang/src/index.ts` (should only export Catalog, Container, Support, and re-exports of Moq/Signals).
- Consider switching `@moq/hang` build from `tsc` to vite lib mode for consistency and jsdelivr support.
- Full build + test pass across all packages.

**README updates:**
- Final rewrite of `hang/README.md`:
  - Update the package description to reflect its new scope (catalog, container, support only).
  - Remove the "Web Components" section entirely (or keep only `<hang-support>`).
  - Replace the "Javascript API" section with catalog/container examples.
  - Update the "Features" list to reflect core-only functionality.
  - Add a "Related Packages" section linking to `@moq/watch`, `@moq/publish`, and `@moq/ui-core`.

**Doc site updates:**
- Rewrite `doc/js/@moq/hang/index.md` to reflect core-only scope (catalog, container, support). Add links to `@moq/watch` and `@moq/publish` pages.
- Update `doc/js/index.md` description of `@moq/hang` in the "Core Libraries" section.
- Update `doc/js/env/web.md` production notes about Vite-only support (now applies to `@moq/watch`/`@moq/publish` instead of `@moq/hang`).
- Update `CLAUDE.md` architecture layer description for `hang`.
- Update root `README.md` description of `@moq/hang` in the package table.

**Exit criteria:** `@moq/hang` is lean (catalog + container + support only). All packages build and tests pass.

---

### Milestone 8: (Future) CDN / jsdelivr support

**Scope:**
- Investigate vite/rollup plugin for proper AudioWorklet bundling (avoiding `toString()` hack).
- Set up `public/` directories in `@moq/watch` and `@moq/publish` for runtime-loadable assets.
- Add configurable `basePath` similar to [Shoelace](https://shoelace.style/getting-started/installation#setting-the-base-path).
- Test serving all packages via jsdelivr.
- Document the CDN usage pattern for consumers.

**Exit criteria:** Packages are usable via `<script>` tag from jsdelivr with no bundler required.
