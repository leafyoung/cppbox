Before first release

1.  bunx @tauri-apps/cli signer generate → paste pubkey into tauri.conf.json, private key → TAURI_PRIVATE_KEY secret.
2.  (optional) Apple/Windows signing secrets for silent installs.
3.  git tag v0.1.0 && git push origin v0.1.0 → CI builds everything + pushes cppbox-sandbox to ghcr.

```bash
bunx @tauri-apps/cli signer generate -w cppbox.key
gh secret set TAURI_PRIVATE_KEY --repo leafyoung/cppbox < cppbox.key
gh secret set TAURI_KEY_PASSWORD --repo leafyoung/cppbox < cppbox.key_password

```

Remaining optional polish (not blocking)

- Retire backend/ + pyproject.toml once you've run the Rust build on your desktop.
- Generate a real icon set (cargo tauri icon <png>) to replace the placeholder.
- A PR-time ci.yml for per-commit cargo build --workspace (currently release-only).

Want me to tackle any of those, or pause here for you to run the desktop build locally?

```

```
