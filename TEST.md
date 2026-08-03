# Test

## Option A — browser (recommended for testing the debugger)

```bash
  cd /var/home/yangye/devv/fin/classroom
  cargo run -p cppbox-core --bin cppbox-server
```

It prints CPPBOX_PORT=<n>; open http://127.0.0.1:<n> in a browser.

## Option B — desktop window (Tauri, embeds the same server)

```bash
  CPPBOX_FRONTEND="$PWD/frontend" cargo run -p cppbox
```
