# frontend_v2

Reserved for the v2 frontend. Empty for now: per
[docs/WASM_PLAN.md](../docs/WASM_PLAN.md), Phase 0 (`src_v2/`) is a
backend-only compiler/runtime spike, and Phase 1 (swapping `sandbox.rs`'s
podman calls for `wasmtime`) is designed to need **zero** frontend changes —
`routes.rs`, `admin.rs`, and `frontend/index.html` stay as they are, since
`sandbox.rs` is already the right abstraction boundary.

This directory starts getting used once a phase actually needs a frontend
change (e.g. if debugging or LSP wiring changes shape later). Until then,
keep working in `frontend/`.
