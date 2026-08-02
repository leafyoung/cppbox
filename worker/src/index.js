import { zipSync, strToU8 } from 'fflate';

// CPPBox submission collector.
// Public: GET / (upload form), POST /submit, GET /health.
// Secret-gated (/admin/*, X-Admin-Secret): key allowlist + R2 object CRUD.
// A submission is stored in R2 as {key}+{ms}.zip with a meta.json inside,
// matching the zip format CPPBox's organize step expects.

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const p = url.pathname;

    if (request.method === 'GET' && (p === '/' || p === '/index.html')) return html(FORM_HTML);
    if (request.method === 'GET' && p === '/health') return json({ ok: true, time: Date.now() });
    if (request.method === 'POST' && p === '/submit') return handleSubmit(request, env);

    if (p.startsWith('/admin/')) {
      if (request.headers.get('X-Admin-Secret') !== env.ADMIN_SECRET) return json({ error: 'unauthorized' }, 401);
      if (request.method === 'POST' && p === '/admin/keys') return handlePushKeys(request, env);
      if (request.method === 'GET' && p === '/admin/list') return handleList(env);
      const m = p.match(/^\/admin\/object\/(.+)$/);
      if (m) {
        const name = decodeURIComponent(m[1]);
        if (request.method === 'GET') return handleGetObject(name, env);
        if (request.method === 'DELETE') return handleDeleteObject(name, env);
      }
    }
    return json({ error: 'not found' }, 404);
  },
};

async function handleSubmit(request, env) {
  let form;
  try { form = await request.formData(); } catch { return json({ error: 'invalid form data' }, 400); }
  const key = String(form.get('key') || '').trim();
  if (!key) return json({ error: 'missing submission key' }, 400);

  const valid = await readValidKeys(env);
  if (!valid.includes(key)) return json({ error: 'invalid or unissued key — contact your instructor' }, 403);

  const files = {};
  let count = 0;
  for (const [, value] of form.entries()) {
    if (!(value && typeof value.size === 'number' && value.arrayBuffer)) continue; // skip non-file fields (key)
    const path = value.webkitRelativePath || value.name;
    if (!path) continue;
    files[path] = new Uint8Array(await value.arrayBuffer());
    count++;
  }
  if (count === 0) return json({ error: 'no files attached' }, 400);

  const counter = Date.now();
  const meta = { key, counter, submitted_at: new Date(counter).toISOString(), files: Object.keys(files) };
  const tree = { 'meta.json': strToU8(JSON.stringify(meta, null, 2)), ...files };
  const zipped = zipSync(tree);
  const name = `${key}+${counter}.zip`;
  await env.SUBMISSIONS.put(name, zipped);
  return json({ ok: true, name, size: zipped.byteLength, files: count });
}

async function readValidKeys(env) {
  const raw = await env.KV.get('valid_keys');
  if (!raw) return [];
  try { const a = JSON.parse(raw); return Array.isArray(a) ? a : []; } catch { return []; }
}

async function handlePushKeys(request, env) {
  const body = await request.json().catch(() => ({}));
  const keys = Array.isArray(body.keys) ? body.keys.filter(k => typeof k === 'string' && k.trim()) : [];
  if (!keys.length) return json({ error: 'no keys provided' }, 400);
  const current = await readValidKeys(env);
  const merged = [...new Set([...current, ...keys])];
  await env.KV.put('valid_keys', JSON.stringify(merged));
  return json({ ok: true, total: merged.length, added: merged.length - current.length });
}

async function handleList(env) {
  // ponytail: no pagination — fine while object count stays well under 1000
  const listed = await env.SUBMISSIONS.list();
  const objects = (listed.objects || []).map(o => ({ name: o.key, size: o.size, uploaded: o.uploaded }));
  return json({ objects });
}

async function handleGetObject(name, env) {
  const obj = await env.SUBMISSIONS.get(name);
  if (!obj) return json({ error: 'not found' }, 404);
  return new Response(obj.body, { headers: { 'Content-Type': 'application/zip', 'Content-Disposition': `attachment; filename="${name}"` } });
}

async function handleDeleteObject(name, env) {
  await env.SUBMISSIONS.delete(name);
  return json({ ok: true });
}

function json(obj, status = 200) {
  return new Response(JSON.stringify(obj), { status, headers: { 'Content-Type': 'application/json' } });
}
function html(s) {
  return new Response(s, { headers: { 'Content-Type': 'text/html; charset=utf-8' } });
}

const FORM_HTML = `<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>CPPBox — Submit Assignment</title>
<style>
  :root { --bg:#0f111a; --s:#1a1d2e; --s2:#252840; --b:#2d3154; --t:#cdd6f4; --d:#6c7086; --a:#89b4fa; --g:#a6e3a1; --r:#f38ba8; }
  * { box-sizing:border-box; } body { margin:0; background:var(--bg); color:var(--t); font:14px/1.5 -apple-system,Segoe UI,Roboto,sans-serif; }
  .wrap { max-width:520px; margin:40px auto; padding:0 16px; }
  h1 { font-size:18px; font-weight:600; } .sub { color:var(--d); font-size:12px; margin-bottom:22px; }
  label { display:block; color:var(--d); font-size:12px; margin:14px 0 6px; }
  input[type=text], input[type=password] { width:100%; background:var(--s); border:1px solid var(--b); color:var(--t); padding:10px 12px; border-radius:6px; font-size:13px; outline:none; font-family:inherit; }
  input[type=file] { width:100%; background:var(--s); border:1px dashed var(--b); color:var(--t); padding:14px 12px; border-radius:6px; }
  input:focus { border-color:var(--a); }
  button { width:100%; margin-top:22px; background:var(--a); color:#0f111a; border:none; padding:11px; border-radius:6px; font-size:14px; font-weight:600; cursor:pointer; }
  button:disabled { opacity:.6; cursor:default; }
  #res { margin-top:16px; font-size:13px; min-height:18px; } .ok { color:var(--g); } .err { color:var(--r); }
  .hint { color:var(--d); font-size:11px; margin-top:8px; }
</style></head><body><div class="wrap">
<h1>CPPBox — Submit Assignment</h1>
<div class="sub">Upload your source files with your submission key.</div>
<form id="f">
  <label for="k">Submission key</label>
  <input id="k" name="key" type="text" autocomplete="off" placeholder="paste the key from your instructor">
  <label for="fi">Source files</label>
  <input id="fi" name="files" type="file" multiple>
  <div class="hint">Select one or more files (.cpp, .h, Makefile…). Filenames are kept flat.</div>
  <button id="b" type="submit">Submit</button>
</form>
<div id="res"></div></div>
<script>
const f=document.getElementById('f'), res=document.getElementById('res'), b=document.getElementById('b');
f.onsubmit=async e=>{ e.preventDefault();
  const key=document.getElementById('k').value.trim();
  if(!key){ res.className='err'; res.textContent='Enter your submission key.'; return; }
  const files=document.getElementById('fi').files;
  if(!files.length){ res.className='err'; res.textContent='Choose at least one file.'; return; }
  const fd=new FormData(); fd.append('key', key);
  for(const fl of files) fd.append('files', fl, fl.name);
  b.disabled=true; res.className=''; res.textContent='Uploading…';
  try{ const r=await fetch('/submit',{method:'POST',body:fd}); const j=await r.json();
    if(r.ok){ res.className='ok'; res.textContent='✓ Submitted ('+j.files+' file'+(j.files>1?'s':'')+', '+(j.size/1024).toFixed(1)+' KB). Reference: '+(j.name||'').slice(0,12)+'…'; f.reset(); }
    else { res.className='err'; res.textContent='✕ '+(j.error||'submission failed'); }
  }catch(err){ res.className='err'; res.textContent='✕ network error'; }
  finally{ b.disabled=false; }
};
</script></body></html>`;
