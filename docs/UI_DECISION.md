# UI toolkit decision: `egui` over `slint`

## What the binary's UI actually is

Silhouette Studio's surface is dominated by:

1. a **zoomable/pannable design canvas** with vector paths, rasters, regmark
   overlays and selection handles;
2. a **layer tree** whose depth is dynamic and user-editable;
3. dense **property panels** (material, force, speed, blade depth, passes);
4. a **cut preview** that animates/outlines the path the machine will follow.

The canvas is the product. Everything else is form controls around it.

## Comparison

| Criterion | `egui` | `slint` |
|---|---|---|
| Canvas with dynamic geometry | `Painter` immediate-mode; trivial to draw arbitrary paths each frame | custom rendering possible, but requires implementing a rendering primitive and passing data through the generated bridge |
| Dynamic layer tree | rebuilt from `Vec<Node>` every frame | declarative `for` over a model; fine, but model sync is more ceremony |
| Iteration speed | edit + `cargo run`; no codegen step | `.slint` compile step; errors surface in generated code |
| Native look | custom-themed, not native | closer to native widgets |
| Accessibility | limited | better on desktop |
| Licensing | MIT / Apache-2.0 | GPLv3 for open source; **commercial licence required for a closed-source desktop app** |
| Dependency weight | large but pure Rust | large, with a compiler + runtime |

## Decision

**`egui`.**

Reasons, in order of weight:

1. **The canvas is the core interaction.** Immediate-mode painting matches the
   problem: paths change every frame during pan/zoom/preview.
2. **Licensing.** This reimplementation is not intended to be GPL, so Slint
   would require a commercial licence. `egui` is MIT/Apache-2.0.
3. **No codegen.** The protocol layer is already byte-sensitive; keeping the UI
   in plain Rust avoids a second build language.
4. **Accessibility is not the differentiator here.** The reference application
   is a mouse-driven canvas tool; native widget fidelity is secondary.

`slint` would be the better choice if the product were form-heavy (settings,
wizards, tables) or targeted embedded panels. It is not.

## Current state

`silhouette-ui` contains only presentation/view-model logic today (no GUI
dependency), so the decision can still be revisited cheaply. The GUI is
milestone M3 in [`ROADMAP.md`](ROADMAP.md).
