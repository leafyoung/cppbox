# Test

## Option A — browser (recommended for testing the debugger)

```bash
cargo run -p cppbox-core --bin cppbox-server
CPPBOX_NO_OPEN=1 cargo run -p cppbox-core --bin cppbox-server   # skip auto-open
```

It prints `CPPBOX_PORT=<n>`; open `http://127.0.0.1:<n>` in a browser.

## Option B — desktop window (Tauri, embeds the same server)

```bash
CPPBOX_FRONTEND="$PWD/frontend" cargo run -p cppbox
```
