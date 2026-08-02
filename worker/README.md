# CPPBox Submission Worker

Always-on collector so the teacher's machine can be offline. Students upload
source files + their submission key to this Worker (public HTTPS); it validates
the key against an allowlist and stores a zip in R2. The teacher's CPPBox
**pulls** the queue when online, then runs the normal Organize → Mark flow.

Cost at classroom scale (<1 MB): **$0** — within R2's 10 GB free tier and
Workers' 100k req/day free tier.

## Deploy (one-time)

```bash
cd worker
npm install

# create the backing resources
npx wrangler login
npx wrangler r2 bucket create cppbox-submissions
npx wrangler kv namespace create KV          # prints an id -> put in wrangler.toml

cp wrangler.toml.example wrangler.toml
# edit wrangler.toml: paste the KV namespace id from above

# set the shared admin secret (CPPBOX_WORKER_SECRET must match)
# generate an unhackable 256-bit secret first, then paste it:
openssl rand -hex 32 | wrangler secret put ADMIN_SECRET
# (or: in CPPBox Admin → Remote collector, click 🎴 to generate, and paste the same value here)

npx wrangler deploy                            # prints https://cppbox-submit.<you>.workers.dev
```

Record the deployed URL and the `ADMIN_SECRET` value — they go into the
teacher's CPPBox environment:

```bash
export CPPBOX_WORKER_URL="https://cppbox-submit.<you>.workers.dev"
export CPPBOX_WORKER_SECRET="<the ADMIN_SECRET you set>"
```

## How keys reach the Worker

When you create an assignment in CPPBox (you're online then), CPPBox pushes the
minted keys to `POST /admin/keys`, writing them to KV. The Worker rejects any
key not in that allowlist, so spam can't fill R2. Keys are 256-bit values from
the OS CSPRNG (`secrets.token_hex(32)`) — not derived from time or any seed, so
unguessable.

## Where the Worker URL+secret live

NOT hardcoded. They're a DB **Setting** editable in CPPBox → Admin → *Remote
collector* (with `CPPBOX_WORKER_URL` / `CPPBOX_WORKER_SECRET` env vars as a
fallback). The value you put in Admin → Remote collector's secret field must
match the Worker's `ADMIN_SECRET`.

## Endpoints

| Method | Path | Auth | Purpose |
|--------|------|------|---------|
| GET  | `/` | none | Upload form for students |
| POST | `/submit` | key allowlist | validate key, zip files, store `{key}+{ms}.zip` in R2 |
| GET  | `/health` | none | liveness |
| POST | `/admin/keys` | secret | append keys to the allowlist |
| GET  | `/admin/list` | secret | list pending R2 objects |
| GET  | `/admin/object/{name}` | secret | download one zip |
| DELETE | `/admin/object/{name}` | secret | delete after pull |

The pull drains R2 (download + delete), so it stays empty between pulls.

## Test locally without Cloudflare

A faithful mock worker (`worker/mock_server.py`) implements the same routes so
you can exercise push → submit → pull → organize with no Cloudflare account:

```bash
python3 worker/mock_server.py 8001   # serves http://localhost:8001
export CPPBOX_WORKER_URL="http://localhost:8001"
export CPPBOX_WORKER_SECRET="dev-secret"
# then in CPPBox: create assignment (pushes keys) -> upload via the form
# -> click Pull in the assignment card -> Organize
```
