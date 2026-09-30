// ── clangd LSP client (lint, completion, go-to-definition, signature help,
// quick-fixes, hover) and the lldb-dap debugger bridge (breakpoints, step
// controls, call stack / variables panel).
import { S } from './state.js';
import { api, esc, setStatus, setDot, icon } from './api.js';
import { focusedCm, findGroupIndexForPath, revealFile, allOpenTabs } from './editor-panes.js';
import { saveAllDirty, renderOutTab } from './projects.js';

// ── Lint (per-pane; CM5's async lint addon passes the originating `cm` as the 4th arg) ─
function diagToAnns(d) {
  const r = d.range || {}; const s = r.start || { line: 0, character: 0 }; const e = r.end || s;
  const from = CodeMirror.Pos(s.line, s.character);
  let to = CodeMirror.Pos(e.line, e.character);
  if (from.line === to.line && from.ch === to.ch) to = CodeMirror.Pos(to.line, to.ch + 1);
  const sev = d.severity === 1 ? 'error' : d.severity === 2 ? 'warning' : 'info';
  return { from, to, message: d.message, severity: sev };
}
let lintTimers = new Map(), lintSeqs = new Map();
export function lintGetAnnotations(text, updateLinting, options, cm) {
  const path = cm ? S.cmPathMap.get(cm) : null;
  const info = path ? S.lsp.docs.get(path) : null;
  // Primary source: clangd (carries compiler errors + clang-tidy hints)
  if (S.lsp.ready && info) {
    updateLinting((S.lspDiags[info.uri] || []).map(diagToAnns));
    return;
  }
  // Fallback before clangd is ready: debounced -fsyntax-only, one timer per pane
  clearTimeout(lintTimers.get(cm));
  lintTimers.set(cm, setTimeout(async () => {
    const seq = (lintSeqs.get(cm) || 0) + 1; lintSeqs.set(cm, seq);
    if (!S.project || !path) { updateLinting([]); return; }
    const files = allOpenTabs().filter(t => !t.missing).map(t => ({ name: baseName(t.path), content: t.doc.getValue() }));
    const entry = baseName(path);
    try {
      const res = await api('POST', '/api/check', { files, std: document.getElementById('stdSelect').value, entry });
      if (lintSeqs.get(cm) !== seq) return;
      const anns = (res.diagnostics || []).filter(d => d.file === entry).map(d => {
        const l = Math.max(0, d.line - 1), c = Math.max(0, d.col - 1);
        return { from: CodeMirror.Pos(l, c), to: CodeMirror.Pos(l, c + 1), message: d.message, severity: d.severity === 'error' ? 'error' : d.severity === 'warning' ? 'warning' : 'info' };
      });
      updateLinting(anns);
    } catch (e) { }
  }, 600));
}
export function refreshLint() { for (const cm of S.cmPool) { clearTimeout(lintTimers.get(cm)); if (cm.performLint) cm.performLint(); } }
function baseName(p) { return (p || '').split('/').pop(); }

// clangd LSP ──────────────────────────────────────────────────────────────
// S.lsp.docs: path -> {uri, version} — one open LSP document per simultaneously
// visible tab (replaces the old single scalar lsp.openUri/lsp.version, which
// assumed only one file was ever open at a time).
function lspUri(path) { return 'file://' + (S.lsp.workspace || '/tmp/cppbox') + '/' + (S.project ? S.project.id + '/' : '') + (path || ''); }
function lspSend(method, params) { const id = S.lsp.reqId++; return new Promise(res => { S.lsp.pending[id] = res; S.lsp.ws.send(JSON.stringify({ jsonrpc: '2.0', id, method, params })); }); }
function lspNotify(method, params) { if (S.lsp.ws && S.lsp.ws.readyState === 1) S.lsp.ws.send(JSON.stringify({ jsonrpc: '2.0', method, params })); }
export function lspConnect() {
  // Re-callable: openProject() calls this on every project switch so each
  // project gets its own $/sync'd workspace and its own S.lsp.docs — closing
  // any previous connection first (and detaching its onclose) so the old
  // connection's auto-reconnect doesn't race with the new one.
  if (S.lsp.ws) { S.lsp.ws.onclose = null; S.lsp.ws.close(); }
  S.lsp.ready = false; S.lsp.docs.clear();
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  S.lsp.ws = new WebSocket(`${proto}://${location.host}/ws/lsp`);
  S.lsp.ws.onopen = async () => {
    try {
      const sync = await lspSend('$/sync', { std: document.getElementById('stdSelect').value, files: syncFiles() });
      S.lsp.workspace = sync.workspace;
      await lspSend('initialize', { processId: null, rootUri: 'file://' + S.lsp.workspace, capabilities: {}, initializationOptions: { fallbackFlags: ['-std=' + document.getElementById('stdSelect').value] } });
      lspNotify('initialized', {}); S.lsp.ready = true;
      S.lsp.docs.clear();
      for (const g of S.groups) { if (g.activeIdx < 0) continue; const t = g.tabs[g.activeIdx]; if (t && !t.missing) ensureLspOpen(t.path, t.doc.getValue()); }
      refreshLint();
      setStatus('clangd ready'); setDot('sbLsp', 'ok');
    } catch (e) { console.warn('lsp init', e); }
  };
  S.lsp.ws.onmessage = ev => {
    const m = JSON.parse(ev.data);
    if (m.id != null && S.lsp.pending[m.id]) { const r = S.lsp.pending[m.id]; delete S.lsp.pending[m.id]; r(m.result); }
    if (m.method === 'textDocument/publishDiagnostics') {
      const uri = m.params && m.params.uri;
      S.lspDiags[uri] = (m.params && m.params.diagnostics) || [];
      for (const cm of S.cmPool) { const path = S.cmPathMap.get(cm); const info = path ? S.lsp.docs.get(path) : null; if (info && info.uri === uri && cm.performLint) cm.performLint(); }
      updateDiagCounts();
      if (S.outTab === 'problems') renderOutTab();
    }
  };
  S.lsp.ws.onclose = () => { S.lsp.ready = false; S.lsp.docs.clear(); setDot('sbLsp', 'bad'); setTimeout(lspConnect, 1500); };
}
function syncFiles() { return S.groups.flatMap(g => g.tabs).filter(t => !t.missing).map(t => ({ name: (S.project ? S.project.id + '/' : '') + t.path, content: t.doc.getValue() })); }
export function ensureLspOpen(path, text) {
  if (!S.lsp.ready || S.lsp.docs.has(path)) return;
  const uri = lspUri(path);
  S.lsp.docs.set(path, { uri, version: 1 });
  lspNotify('textDocument/didOpen', { textDocument: { uri, languageId: 'cpp', version: 1, text } });
}
export function lspDidChangePath(path, text) {
  const info = S.lsp.docs.get(path); if (!S.lsp.ready || !info) return;
  info.version++;
  lspNotify('textDocument/didChange', { textDocument: { uri: info.uri, version: info.version }, contentChanges: [{ text }] });
}
export function lspDidClose(path) {
  const info = S.lsp.docs.get(path); if (!info) return;
  S.lsp.docs.delete(path);
  lspNotify('textDocument/didClose', { textDocument: { uri: info.uri } });
}
export function clangHint(cm) {
  const path = S.cmPathMap.get(cm); const info = path ? S.lsp.docs.get(path) : null;
  const cur = cm.getCursor(), tok = cm.getTokenAt(cur), from = CodeMirror.Pos(cur.line, tok.start);
  if (!info) return Promise.resolve({ list: [], from, to: cur });
  return lspSend('textDocument/completion', { textDocument: { uri: info.uri }, position: { line: cur.line, character: cur.ch } }).then(res => {
    let items = res || []; if (items && items.items) items = items.items;
    if (!items.length) return { list: [], from, to: cur };
    return { from, to: cur, list: items.map(it => ({ text: it.insertText || it.label, displayText: it.label, detail: it.detail })) };
  }).catch(() => ({ list: [], from, to: cur }));
}

// ── clangd: go-to-definition (F12 / Ctrl+click), signature help, quick-fixes ─
// Every function here resolves "the current file" from the CodeMirror instance
// that triggered it (S.cmPathMap), not a single global — so keybindings/hover/
// quick-fix act on whichever pane has focus.
export async function gotoDefinition(cm) {
  cm = cm || focusedCm();
  const path = S.cmPathMap.get(cm); const info = path ? S.lsp.docs.get(path) : null;
  if (!S.lsp.ready || !info) return;
  const gi = S.cmPool.indexOf(cm); if (gi >= 0) S.focusedGroup = gi;
  const cur = cm.getCursor();
  try {
    const res = await lspSend('textDocument/definition', { textDocument: { uri: info.uri }, position: { line: cur.line, character: cur.ch } });
    const loc = Array.isArray(res) ? res[0] : res; if (!loc || !loc.uri) return setStatus('No definition found');
    const prefix = 'file://' + (S.lsp.workspace || '') + '/' + (S.project ? S.project.id + '/' : '');
    if (!loc.uri.startsWith(prefix)) return setStatus('Definition outside project');
    const path2 = loc.uri.slice(prefix.length);
    // Always go through revealFile — even for a same-file jump — rather than
    // calling cm.setCursor directly on the `cm` captured before this await:
    // if the user switched tabs in this pane during the round-trip, `cm` now
    // shows a different Doc, and revealFile re-resolves which pane (if any)
    // actually shows path2 instead of trusting the stale reference.
    await revealFile(path2, { line: loc.range.start.line, ch: loc.range.start.character });
  } catch (_) { setStatus('Definition not available'); }
}
let sigEl = document.createElement('div'); sigEl.className = 'lsp-hover'; sigEl.style.display = 'none'; document.body.appendChild(sigEl);
export async function showSignature(cm) {
  const path = S.cmPathMap.get(cm); const info = path ? S.lsp.docs.get(path) : null;
  if (!S.lsp.ready || !info) return;
  const cur = cm.getCursor();
  try {
    const res = await lspSend('textDocument/signatureHelp', { textDocument: { uri: info.uri }, position: { line: cur.line, character: cur.ch } });
    if (!res || !res.signatures || !res.signatures.length) { sigEl.style.display = 'none'; return; }
    const sig = res.signatures[res.activeSignature || 0]; if (!sig) { sigEl.style.display = 'none'; return; }
    let params = (sig.parameters || []).map((p, i) => i === (res.activeParameter || 0) ? '«' + (p.label || '') + '»' : (p.label || '')).join(', ');
    sigEl.innerHTML = '<b>' + esc(sig.label || '') + '</b><br>' + esc(params);
    const pos = cm.cursorCoords(true, 'window');
    sigEl.style.left = Math.min(pos.left, window.innerWidth - 460) + 'px';
    sigEl.style.top = Math.max(4, pos.top - 56) + 'px';
    sigEl.style.display = 'block';
    clearTimeout(sigEl._t); sigEl._t = setTimeout(() => sigEl.style.display = 'none', 6000);
  } catch (_) { sigEl.style.display = 'none'; }
}
export async function quickFixMenu(cm) {
  cm = cm || focusedCm();
  const path = S.cmPathMap.get(cm); const info = path ? S.lsp.docs.get(path) : null;
  if (!S.lsp.ready || !info) return;
  const cur = cm.getCursor();
  const lineDiags = (S.lspDiags[info.uri] || []).filter(d => { const r = d.range || {}; const s = r.start || { line: 0 }, e = r.end || s; return cur.line >= s.line && cur.line <= e.line; });
  if (!lineDiags.length) return setStatus('No problems on this line');
  const rng = lineDiags[0].range;
  try {
    const res = await lspSend('textDocument/codeAction', { textDocument: { uri: info.uri }, range: rng, context: { diagnostics: lineDiags } });
    if (!res || !res.length) return setStatus('No quick fixes available');
    let html = res.map((a, i) => `<div class="pal-item" onclick="applyQuickFix(${i})">${esc(a.title || 'fix')}</div>`).join('');
    window._qf = res; window._qfCm = cm;
    const m = document.getElementById('qfPop');
    const pos = cm.cursorCoords(true, 'window');
    m.style.left = Math.min(pos.left - 10, window.innerWidth - 320) + 'px'; m.style.top = (pos.bottom + 6) + 'px';
    m.innerHTML = html; m.style.display = 'block';
    setTimeout(() => document.addEventListener('mousedown', function h(ev) { if (!ev.target.closest('#qfPop')) { m.style.display = 'none'; document.removeEventListener('mousedown', h); } }), 0);
  } catch (_) { setStatus('Quick fix not available'); }
}
export function applyQuickFix(i) {
  const a = window._qf && window._qf[i]; const cm = window._qfCm; document.getElementById('qfPop').style.display = 'none';
  if (!a) return;
  if (a.edit) applyWorkspaceEdit(a.edit, cm);
  else if (a.command) setStatus('Fix command: ' + a.command.title + ' (not auto-applied)');
}
function applyWorkspaceEdit(we, cm) {
  cm = cm || focusedCm();
  if (!we.changes) return setStatus('Unsupported fix format');
  const path = S.cmPathMap.get(cm); const info = path ? S.lsp.docs.get(path) : null;
  for (const uri in we.changes) {
    if (!info || uri !== info.uri) continue;   // ponytail: same-file fixes only; cross-file needs reopen logic
    // descending line, then descending column: applying edits right-to-left
    // on each line (not just bottom-to-top) so an earlier-applied edit never
    // shifts the column offset a later edit on the SAME line still relies on.
    const edits = we.changes[uri].sort((x, y) => (y.range.start.line - x.range.start.line) || (y.range.start.character - x.range.start.character));
    cm.operation(() => { for (const e of edits) { const s = e.range.start, en = e.range.end; cm.replaceRange(e.newText, { line: s.line, ch: s.character }, { line: en.line, ch: en.character }); } });
    setStatus('Quick fix applied');
    return;
  }
  setStatus('Fix edits another file — reopen it to see changes');
}

let hoverEl = document.createElement('div'); hoverEl.className = 'lsp-hover'; hoverEl.style.display = 'none'; document.body.appendChild(hoverEl);
let hoverTimer = null;
export function hideHoverPopup() { hoverEl.style.display = 'none'; }
export function onEditorMouseMove(cm, e) {
  if (!S.lsp.ready) return;
  clearTimeout(hoverTimer);
  hoverTimer = setTimeout(async () => {
    const path = S.cmPathMap.get(cm); const info = path ? S.lsp.docs.get(path) : null; if (!info) { hoverEl.style.display = 'none'; return; }
    const pos = cm.coordsChar({ left: e.clientX, top: e.clientY }, 'page'); if (!pos || pos.outside) { hoverEl.style.display = 'none'; return; }
    try {
      const res = await lspSend('textDocument/hover', { textDocument: { uri: info.uri }, position: { line: pos.line, character: pos.ch } });
      if (!res || !res.contents) { hoverEl.style.display = 'none'; return; }
      let t = res.contents; if (typeof t === 'object') t = t.value; if (!t || !t.trim()) { hoverEl.style.display = 'none'; return; }
      hoverEl.textContent = t; hoverEl.style.left = (e.clientX + 14) + 'px'; hoverEl.style.top = (e.clientY + 14) + 'px'; hoverEl.style.display = 'block';
    } catch (_) { hoverEl.style.display = 'none'; }
  }, 450);
  hoverEl.style.left = (e.clientX + 14) + 'px'; hoverEl.style.top = (e.clientY + 14) + 'px';
}

// ── Status bar diagnostic counts ─────────────────────────────────────────
export function updateDiagCounts() {
  let e = 0, w = 0;
  for (const u in S.lspDiags) for (const d of S.lspDiags[u] || []) { if (d.severity === 1) e++; else if (d.severity === 2) w++; }
  const el = document.getElementById('sbDiags');
  el.textContent = (e ? '✕ ' + e : '') + (w ? (e ? ' ' : '') + '⚠ ' + w : '') || '✓ no problems';
  el.style.color = e ? 'var(--red)' : w ? 'var(--yellow)' : '';
}

// ── Debugger (lldb-dap via /ws/debug) ───────────────────────────────────
let debugWs = null, debugState = 'idle';        // idle|starting|running|stopped
let debugBps = {};                            // file -> Set of 1-based lines (global — not per-pane)
let debugLineMarker = null;
// 'breakpoints' gutter is enabled via the editor `gutters` option; no define needed (CM5)
function mkBpMarker() { const d = document.createElement('div'); d.className = 'cm-debug-bp'; d.title = 'Breakpoint'; return d; }
export function toggleBreakpoint(cm, path, line) {
  if (!path || !S.project) return;
  const set = debugBps[path] || (debugBps[path] = new Set());
  const ln = line + 1;
  if (set.has(ln)) { set.delete(ln); cm.setGutterMarker(line, 'breakpoints', null); }
  else { set.add(ln); cm.setGutterMarker(line, 'breakpoints', mkBpMarker()); }
  if (debugWs && (debugState === 'running' || debugState === 'stopped')) sendDebug({ cmd: 'Bp', file: path, lines: [...set] });
}
// `debugBps` (keyed by file path) is the durable source of truth: gutter markers
// are re-rendered imperatively from it every time a Doc attaches to a pane
// (attachDocToPane → renderBpGutterFor), so marker persistence across swapDoc /
// pane moves does not depend on CM5 preserving them on its own.
export function renderBpGutterFor(cm, path) {
  cm.clearGutter('breakpoints');
  const set = debugBps[path]; if (!set) return;
  for (const ln of set) cm.setGutterMarker(ln - 1, 'breakpoints', mkBpMarker());
}
function updateDebugUi() {
  const t = document.getElementById('debugToolbar'); if (!t) return;
  const active = debugState !== 'idle';
  t.style.display = active ? 'flex' : 'none';
  document.getElementById('dbgBtn').innerHTML = active ? icon('stop') + ' Stop Debug' : icon('bug') + ' Debug';
  const p = document.getElementById('dbgPanel'); if (p) p.style.display = active ? 'block' : 'none';
  const st = document.getElementById('debugStatus');
  if (st) st.textContent = active ? (debugState === 'starting' ? 'compiling…' : debugState === 'stopped' ? 'paused' : 'running') : '';
  // disable-when-inactive
  const set = (id, en) => { const b = document.getElementById(id); if (b) b.disabled = !en; };
  set('dbgStart', debugState === 'idle');
  const paused = debugState === 'stopped';
  set('dbgContinue', paused); set('dbgNext', paused); set('dbgStepIn', paused); set('dbgStepOut', paused);
  set('dbgStop', active);
}
export function debugToggle() { if (debugState === 'idle') debugStart(); else debugCmd('Stop'); }
export async function debugStart() {
  if (!S.project) { setStatus('Open a project first'); return; }
  await saveAllDirty();   // debug compiles from disk
  debugOutputClear(); setDebugLine(null, null);
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  const std = document.getElementById('stdSelect').value;
  debugWs = new WebSocket(`${proto}://${location.host}/ws/debug?pid=${S.project.id}&std=${std}`);
  debugState = 'starting'; updateDebugUi();
  debugWs.onmessage = e => { let m; try { m = JSON.parse(e.data); } catch { return; } debugOnMsg(m); };
  debugWs.onclose = () => { if (debugState !== 'idle') { debugState = 'idle'; setDebugLine(null, null); updateDebugUi(); } };
  debugWs.onopen = () => sendDebug({ cmd: 'Start', breakpoints: debugBpMap() });
}
function sendDebug(o) { if (debugWs && debugWs.readyState === 1) debugWs.send(JSON.stringify(o)); }
function debugBpMap() { const o = {}; for (const f in debugBps) o[f] = [...debugBps[f]]; return o; }
function debugOnMsg(m) {
  switch (m.type) {
    case 'status': setDebugStatus(m.text); break;
    case 'error': setDebugStatus('⚠ ' + m.msg); debugState = 'idle'; updateDebugUi(); break;
    case 'breakpoints': applyBpVerification(m.file, m.results); break;
    case 'running': debugState = 'running'; setDebugLine(null, null); clearDebugInfo(); updateDebugUi(); break;
    case 'stopped': debugState = 'stopped'; updateDebugUi();
      if (m.file) { setDebugLine(m.file, (m.line || 1) - 1); } else setDebugLine(null, null);
      break;
    case 'debug_info': renderStack(m.frames || []); renderVars(m.vars || [], document.getElementById('dbgVars')); break;
    case 'vars': { const el = dbgVarExpand[m.ref]; if (el) { renderVars(m.vars || [], el); } break; }
    case 'output': debugOutput(m.text); break;
    case 'exited': debugOutput('\n[exited code ' + (m.code != null ? m.code : '?') + ']\n'); setDebugLine(null, null); clearDebugInfo(); break;
    case 'ended': debugState = 'idle'; setDebugLine(null, null); clearDebugInfo(); updateDebugUi(); break;
  }
}
function applyBpVerification(file, results) {
  const set = debugBps[file]; if (!set || !results) return;
  for (const r of results) {
    if (r.ok === false && r.line != null) {
      set.delete(r.line);
      const gi = findGroupIndexForPath(file);
      if (gi >= 0) S.cmPool[gi].setGutterMarker(r.line - 1, 'breakpoints', null);
    }
  }
}
// reveal the stopped-at file (in whichever pane it's already open, or into the
// focused pane if not open anywhere yet) and mark the current-line highlight
async function setDebugLine(path, line0) {
  if (debugLineMarker) { debugLineMarker.clear(); debugLineMarker = null; }
  if (path == null || line0 == null) return;
  await revealFile(path);
  const gi = findGroupIndexForPath(path); if (gi < 0) return;
  const cm = S.cmPool[gi];
  if (line0 >= 0 && line0 < cm.lineCount()) {
    debugLineMarker = cm.markText({ line: line0, ch: 0 }, { line: line0, ch: 0 }, { className: 'cm-debug-current' });
    cm.setCursor({ line: line0, ch: 0 });
  }
}
export function debugCmd(cmd) {
  if (cmd === 'Stop') { sendDebug({ cmd: 'Stop' }); debugState = 'idle'; setDebugLine(null, null); updateDebugUi(); return; }
  sendDebug({ cmd });
}
// called from openProject (projects.js) when switching projects — breakpoints
// are keyed by file path only, so a stale current-line marker / bp map from
// the previous project should not bleed into the newly-opened one.
export function resetDebugState() { debugBps = {}; setDebugLine(null, null); }
// called from renameNode (projects.js) — debugBps is keyed by path, and
// renaming a file doesn't otherwise migrate its breakpoints to the new path.
export function renameDebugBreakpoints(oldPath, newPath) {
  if (debugBps[oldPath]) { debugBps[newPath] = debugBps[oldPath]; delete debugBps[oldPath]; }
}
function debugOutput(t) { const el = document.getElementById('debugOutput'); el.style.display = 'block'; el.textContent += t; el.scrollTop = el.scrollHeight; }
function debugOutputClear() { const el = document.getElementById('debugOutput'); el.textContent = ''; el.style.display = 'none'; }
function setDebugStatus(s) { const el = document.getElementById('debugStatus'); if (el) el.textContent = s; }
// ── debug info: call stack + variables ──────────────────────────────────
let dbgVarExpand = {};   // variablesReference -> container element
function clearDebugInfo() { const s = document.getElementById('dbgStack'), v = document.getElementById('dbgVars'); if (s) s.innerHTML = ''; if (v) v.innerHTML = ''; dbgVarExpand = {}; }
function renderStack(frames) {
  const el = document.getElementById('dbgStack'); if (!el) return;
  el.innerHTML = '';
  for (const f of frames) {
    const row = document.createElement('div'); row.className = 'dbg-frame';
    row.textContent = `${f.name || '?'} — ${f.file || '?'}:${f.line != null ? f.line : '?'}`;
    row.onclick = () => { if (f.file) setDebugLine(f.file, (f.line || 1) - 1); };
    el.appendChild(row);
  }
}
function renderVars(vars, container) {
  if (!container) return;
  container.innerHTML = '';
  for (const v of vars) appendVar(v, container);
}
function appendVar(v, container) {
  const row = document.createElement('div'); row.className = 'dbg-var';
  const name = document.createElement('span'); name.className = 'dbg-vname'; name.textContent = (v.name != null ? v.name : '?');
  const val = document.createElement('span'); val.className = 'dbg-vval'; val.textContent = (v.value != null ? v.value : '');
  row.appendChild(name); row.appendChild(val); container.appendChild(row);
  if (v.ref > 0) {
    const kids = document.createElement('div'); kids.className = 'dbg-vkids'; kids.style.display = 'none';
    row.classList.add('expandable');
    row.onclick = () => {
      const open = kids.style.display !== 'none';
      kids.style.display = open ? 'none' : 'block';
      row.classList.toggle('open', !open);
      if (!open) { dbgVarExpand[v.ref] = kids; sendDebug({ cmd: 'Expand', ref: v.ref }); }
    };
    container.appendChild(kids);
  }
}

// exposed for inline HTML handlers
Object.assign(window, { applyQuickFix, debugCmd, debugStart, debugToggle });
