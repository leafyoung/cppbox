# CPPBox

A local-first C++ teaching IDE, shipped as a single Tauri desktop binary. One
app, two sides: **students** write, build, debug, test, and submit C++
projects; **teachers** manage classes/assignments and mark submissions.

```bash
# dev run — desktop window (Tauri), backend embedded in-process
CPPBOX_FRONTEND="$PWD/frontend" cargo run -p cppbox
```

## Docs

- [DEPLOY.md](DEPLOY.md) — architecture, running locally, build/release, one-time setup
- [docs/TEST.md](docs/TEST.md) — running the app for manual testing
- [docs/DESIGN_SUBMISSION.md](docs/DESIGN_SUBMISSION.md) — submission & marking system design
- [docs/TODO.md](docs/TODO.md) — deferred items and open work
- [worker/README.md](worker/README.md) — the Cloudflare Worker submission collector

See [CLAUDE.md](CLAUDE.md) for the full architecture/commands reference used
by AI coding agents in this repo.
