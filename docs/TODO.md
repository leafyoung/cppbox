# TODO / Ponytail Deferred Items

UI review round 1 (U1–U6: state coherence, toolbar/icons, bottom panel,
sidebar, submit confirm/a11y, pedagogy upgrades) is fully shipped — verified
against the code, so removed from this list. Real role gating still needs
auth (see the deferred item below).

Items deliberately skipped for now. Add when the requirement becomes concrete.
Status labels: `[DONE]` shipped · `[DEFERRED]` deliberately parked until its "when" fires · `[N/A]` not actually a gap.

- **User auth** `[DEFERRED]`: no login/accounts. Projects are global on one machine (desktop app). Add when multi-user needed.
- **HTTPS** `[DEFERRED]`: app serves localhost only; behind a reverse proxy (nginx/caddy) if ever exposed. Add when deployed to prod.
- **File uploads** `[DEFERRED]`: files come from the on-disk project tree; submissions arrive via the Cloudflare collector. Add browser upload when needed.
- **Docker image optimization** `[DEFERRED]`: Alpine clang image is ~610MB (now incl. gdb/lldb). A multi-stage build could slim it. Add when image size matters.
- **CodeMirror 6** `[DEFERRED]`: CM5 from CDN (one script tag). Upgrade to CM6 when more editor features needed. (Explicitly out of scope per user.)
- **clangd session lifecycle** `[DEFERRED]`: one clangd per WebSocket session (no pooling). Pool/reuse if spin-up becomes noticeable under load.
- **C++23 keyword set == C++20** `[N/A]`: C++23 adds no new core keywords beyond C++20, so highlighting is identical for 20/23. Accurate, not a gap.
- **SSH/Remote-SSH bootstrap** `[DEFERRED]`: host exposes SSH (ap308:22); the user configures their VS Code Remote-SSH host entry. No auto keygen/config write yet.
- **Theme coverage** `[DEFERRED]`: CM editor + app CSS variables are themed per scheme; not every pixel (e.g. modal accents) is tuned. Expand when a scheme looks off.

## Before first release

1. `[DEFERRED]` (optional) Apple/Windows code-signing secrets for silent installs — currently ships unsigned (macOS right-click→Open once; Windows SmartScreen click-through).
