// ── API fetch helper, toast/retry UI, ask() dialog, and small shared
// utilities (esc/icon/timeAgo/setStatus/flashErr/copyText) used by every
// other module. Leaf module — no imports of its own.

// ── API (network failure → toast + Retry; the call resumes when retried) ─
export async function api(method, url, body) {
  const o = { method, headers: {} };
  if (body !== undefined) { o.headers['Content-Type'] = 'application/json'; o.body = JSON.stringify(body); }
  let r;
  try { r = await fetch(url, o); }
  catch (_) {
    return new Promise((resolve, reject) => {
      toast("Can't reach the CPPBox backend — is the app still running?", 'error', {
        label: 'Retry',
        fn: async () => { try { resolve(await readResp(await fetch(url, o))); } catch (e2) { reject(e2); } },
      });
    });
  }
  return readResp(r);
}
export async function readResp(r) {
  if (!r.ok) { let e; try { e = (await r.json()).detail; } catch (_) { e = r.statusText; } throw new Error(e); }
  return r.json();
}

// ── Ask dialog (replaces window.prompt) ────────────────────────────────
let askResolve = null;
export function ask(title, label, value = '', hint = '') {
  return new Promise(res => {
    askResolve = res;
    document.getElementById('askTitle').textContent = title;
    document.getElementById('askLabel').textContent = label;
    document.getElementById('askHint').textContent = hint;
    const inp = document.getElementById('askInput'); inp.value = value;
    document.getElementById('askModal').classList.add('show');
    setTimeout(() => { inp.focus(); inp.select(); }, 0);
  });
}
export function askOk() { const v = document.getElementById('askInput').value; document.getElementById('askModal').classList.remove('show'); if (askResolve) { askResolve(v); askResolve = null; } }
export function askCancel() { document.getElementById('askModal').classList.remove('show'); if (askResolve) { askResolve(null); askResolve = null; } }

// ── Utils ────────────────────────────────────────────────────────────────
// HTML-escapes s AND quote characters, since callers interpolate this into
// single-quoted JS string literals inside double-quoted HTML attributes
// (e.g. onclick="foo(event,'${esc(path)}')") — a bare "'" would otherwise
// terminate that literal early and corrupt the generated handler.
export function esc(s) { const d = document.createElement('div'); d.textContent = s == null ? '' : s; return d.innerHTML.replace(/'/g, '&#39;').replace(/"/g, '&quot;'); }

// ── inline SVG icon set (no deps) ───────────────────────────────────────
export const ICONS = {
  play: '<path d="M7 5v14l12-7z"/>',
  stop: '<rect x="7" y="7" width="10" height="10"/>',
  bug: '<rect x="8" y="9" width="8" height="10" rx="4"/><path d="M12 9V6"/><path d="M9 6L7 4M15 6l2-2"/><path d="M8 12H5M8 16H5M16 12h3M16 16h3"/>',
  wand: '<path d="M4 20L14 10"/><path d="M15 4v3M13.5 5.5h3M19 9v3M17.5 10.5h3"/>',
  save: '<path d="M5 3h11l3 3v15H5z"/><path d="M8 3v5h7V3"/><rect x="8" y="13" width="8" height="5"/>',
  refresh: '<path d="M20 12a8 8 0 1 1-2.34-5.66"/><path d="M20 3v6h-6"/>',
  upload: '<path d="M12 16V4"/><path d="M6 10l6-6 6 6"/><path d="M4 20h16"/>',
  code: '<path d="M8 6l-5 6 5 6"/><path d="M16 6l5 6-5 6"/>',
  book: '<path d="M3 5c3-1 6-1 9 1 3-2 6-2 9-1v13c-3-1-6-1-9 1-3-2-6-2-9-1z"/><path d="M12 6v14"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  file: '<path d="M14 3H6v18h12V7z"/><path d="M14 3v4h4"/>',
  folder: '<path d="M3 7l2-2h5l2 2h9v12H3z"/>',
  fileplus: '<path d="M14 3H6v18h12V7z"/><path d="M14 3v4h4"/><path d="M12 11v6M9 14h6"/>',
  folderplus: '<path d="M3 7l2-2h5l2 2h9v12H3z"/><path d="M12 11v6M9 14h6"/>',
  pencil: '<path d="M17 3l4 4L8 20H4v-4z"/>',
  home: '<path d="M3 11l9-8 9 8"/><path d="M5 10v10h14V10"/>',
  folderopen: '<path d="M3 7l2-2h5l2 2h5v3H3z"/><path d="M3 10l2 9h14l2-9z"/>',
  trash: '<path d="M4 7h16"/><path d="M9 7V4h6v3"/><path d="M6 7l1 13h10l1-13"/>',
  chevron: '<path d="M9 6l6 6-6 6"/>',
  settings: '<circle cx="12" cy="12" r="3"/><path d="M12 3v3M12 18v3M4.2 7.5l2.6 1.5M17.2 15l2.6 1.5M4.2 16.5l2.6-1.5M17.2 9l2.6-1.5"/>',
  next: '<path d="M6 4l8 8-8 8"/><path d="M18 4v16"/>',
  stepin: '<path d="M12 4v12"/><path d="M8 12l4 4 4-4"/><path d="M5 20h14"/>',
  stepout: '<path d="M12 20V8"/><path d="M8 12l4-4 4 4"/><path d="M5 20h14"/>',
  restore: '<path d="M9 14L4 9l5-5"/><path d="M4 9h11a5 5 0 0 1 0 10h-3"/>',
  grad: '<path d="M2 9l10-5 10 5-10 5z"/><path d="M6 11v5c0 1.5 2.7 3 6 3s6-1.5 6-3v-5"/>',
};
export function icon(n, s = 13) { return `<svg width="${s}" height="${s}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${ICONS[n] || ''}</svg>`; }

// ── toasts ───────────────────────────────────────────────────────────────
export function toast(msg, kind = 'info', action) {
  const w = document.getElementById('toastWrap');
  const t = document.createElement('div'); t.className = 'toast' + (kind === 'error' ? ' error' : ''); t.setAttribute('role', 'alert');
  const s = document.createElement('span'); s.style.flex = '1'; s.textContent = msg; t.appendChild(s);
  if (action) { const b = document.createElement('button'); b.className = 'btn btn-ghost'; b.textContent = action.label; b.onclick = () => { t.remove(); action.fn(); }; t.appendChild(b); }
  const x = document.createElement('button'); x.className = 'btn btn-ghost'; x.setAttribute('aria-label', 'Dismiss'); x.innerHTML = '✕'; x.onclick = () => t.remove(); t.appendChild(x);
  w.appendChild(t);
  setTimeout(() => t.remove(), kind === 'error' ? 9000 : 3500);
}

let statusTimer = null;
export function setStatus(m) { document.getElementById('statusText').textContent = m; clearTimeout(statusTimer); if (m) statusTimer = setTimeout(() => { document.getElementById('statusText').textContent = ''; }, 4000); }
export function flashErr(e) { const m = (e && e.message) ? e.message : String(e); setStatus('⚠ ' + m); console.error('[CPPBox]', e); }
window.addEventListener('unhandledrejection', ev => { flashErr(ev.reason); ev.preventDefault(); });
window.addEventListener('error', ev => { if (ev.error) flashErr(ev.error); });
export function timeAgo(iso) { if (!iso) return ''; const ms = Date.now() - new Date(iso).getTime(); const mn = Math.floor(ms / 60000); if (mn < 1) return 'just now'; if (mn < 60) return mn + 'm ago'; const h = Math.floor(mn / 60); if (h < 24) return h + 'h ago'; return new Date(iso).toLocaleDateString(); }

export function copyText(t) { navigator.clipboard && navigator.clipboard.writeText(t); setStatus('Copied'); }

// ── Status bar dot (backend / clangd) ───────────────────────────────────
export function setDot(id, cls) { const el = document.getElementById(id); if (el) el.className = 'dot ' + (cls || ''); }

// exposed for inline HTML handlers (onclick="askOk()" etc.)
Object.assign(window, { askOk, askCancel, copyText });
