// ── Shared mutable state ────────────────────────────────────────────────
// A single object so every module can read AND reassign these fields
// (ES module `import {x}` bindings are read-only live bindings — they
// cannot be reassigned from outside the module that declares them with
// `let x`). Anything reassigned wholesale, or read/written from more than
// one feature module, lives here. Everything else (module-private state:
// `projects`, `trash`, `vscodeInfo`, `slotOrder`, timers, admin/marker
// state, debugger state, etc.) stays a local `let`/`const` inside the
// module that owns it.
//
// This module has no imports of its own — it is a leaf so every other
// module can safely import it without creating a cycle through here.
export const S = {
  project: null,       // currently-open project (or null)
  tree: null,           // current project's file tree
  groups: [],            // pane-grid tab-groups (g0/g1/g2) — set by editor-panes.js at load
  focusedGroup: 0,        // index into groups — which slot last had editor focus
  cmPool: [],              // CodeMirror instances, 1:1 with groups
  cmPathMap: new WeakMap(),// CodeMirror instance -> currently-attached path (or null)
  lsp: { ws: null, ready: false, workspace: null, reqId: 1, pending: {}, docs: new Map() }, // docs: path -> {uri,version}
  lspDiags: {},            // uri -> diagnostics[]
  outTab: 'output',        // active tab in the bottom Output pane
};
