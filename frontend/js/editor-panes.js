// ── Editor panes: CodeMirror pool, C++ mode config, the pane-grid layout
// (up to 3 editor panes + the permanent Output pane), per-pane file tabs,
// drag-and-drop between panes, and pane-layout persistence.
//
// groups[i] is a fixed-identity editor "slot" (g0/g1/g2), always mapped 1:1
// to S.cmPool[i]. Each group is itself a small tab-group: {id, tabs, activeIdx}.
// tabs entries: {path, doc:CodeMirror.Doc|null, dirty, missing}. `doc` holds
// the content permanently — there is no separate flush-on-switch step.
import { S } from './state.js';
import { api, esc, setStatus } from './api.js';
import { expandAncestors, scheduleAutoSave, saveActiveFile, saveAndFormat, runCode, renderTree } from './projects.js';
import { ensureLspOpen, lspDidClose, lspDidChangePath, lintGetAnnotations, clangHint, gotoDefinition, showSignature, quickFixMenu, onEditorMouseMove, toggleBreakpoint, renderBpGutterFor, debugToggle, hideHoverPopup } from './lsp-debug.js';

// ── C++ keyword sets by version ──────────────────────────────────────────
const KW_BASE = 'alignas alignof and and_eq asm auto bitand bitor bool break case catch char char16_t char32_t class compl const const_cast constexpr continue decltype default delete do double dynamic_cast else enum explicit export extern false float for friend goto if inline int long mutable namespace new noexcept not not_eq nullptr operator or or_eq private protected public register reinterpret_cast return short signed sizeof static static_assert static_cast struct switch template this thread_local throw true try typedef typeid typename union unsigned using virtual void volatile wchar_t while xor xor_eq'.split(' ');
const KW_CPP20 = KW_BASE.concat('char8_t concept requires co_await co_return co_yield consteval constinit'.split(' '));
const KW_CPP23 = KW_CPP20.slice(); // C++23 adds no new core keywords beyond C++20
function kwObj(std) { const a = std === 'c++17' ? KW_BASE : std === 'c++20' ? KW_CPP20 : KW_CPP23; const o = {}; a.forEach(k => o[k] = true); return o; }
export function cppMode(std) { return { name: 'text/x-c++src', keywords: kwObj(std), types: { 'std': true }, builtin: { 'std': true } }; }

// ── Pane grid state ─────────────────────────────────────────────────────
const MAX_GROUPS = 3;
function mkGroup(id) { return { id, tabs: [], activeIdx: -1 }; }
S.groups = [mkGroup('g0'), mkGroup('g1'), mkGroup('g2')];
S.focusedGroup = 0;          // index into S.groups — which slot last had editor focus
// slotOrder: visual left-to-right/top-to-bottom order of pane keys ('g0'..'g2','output').
// Empty groups are simply filtered out of the visible order at render time —
// group *identity* (and its S.cmPool binding) never changes, only its screen position.
// (module-private: not read or written from any other feature module)
let slotOrder = ['g0', 'g1', 'g2', 'output'];
const AREA_LETTERS = ['a', 'b', 'c', 'd'];

function cmOptions() {
  // fresh object every call — gutters array / hintOptions / lint must not be shared across instances
  return {
    mode: cppMode(document.getElementById('stdSelect').value), theme: 'material-ocean', lineNumbers: true, indentUnit: 2, tabSize: 2,
    gutters: ['breakpoints', 'CodeMirror-lint-markers'], hintOptions: { hint: clangHint, completeSingle: false },
    lint: { async: true, getAnnotations: lintGetAnnotations },
    extraKeys: {
      'Ctrl-Enter': () => runCode(), 'Cmd-Enter': () => runCode(), 'Shift-Ctrl-Enter': () => runCode(true), 'Shift-Cmd-Enter': () => runCode(true),
      'Ctrl-S': (cm) => saveActiveFile(cm), 'Cmd-S': (cm) => saveActiveFile(cm),
      'Ctrl-F': (cm) => saveAndFormat(cm), 'Cmd-F': (cm) => saveAndFormat(cm), 'Ctrl-Space': 'autocomplete', 'Shift-Space': 'autocomplete',
      'F5': () => debugToggle(), 'F12': (cm) => gotoDefinition(cm), 'Ctrl-.': (cm) => quickFixMenu(cm), 'Cmd-.': (cm) => quickFixMenu(cm)
    },
  };
}
for (let i = 0; i < MAX_GROUPS; i++) {
  const cm = CodeMirror.fromTextArea(document.getElementById('cmHost-g' + i), cmOptions());
  if (i === 0) cm.focus();
  S.cmPool.push(cm);
  wireCmInstance(cm, i);
}

// ── Pane grid: layout, rendering, drag-and-drop ──────────────────────────
export function groupIndex(id) { return S.groups.findIndex(g => g.id === id); }
export function groupById(id) { return S.groups.find(g => g.id === id); }
export function populatedGroups() { return S.groups.filter(g => g.tabs.length > 0); }
export function allOpenTabs() { return S.groups.flatMap(g => g.tabs); }
export function allOpenPaths() { return allOpenTabs().filter(t => !t.missing).map(t => t.path); }
export function focusedCm() { return S.cmPool[S.focusedGroup]; }
export function focusedTabInfo() { const g = S.groups[S.focusedGroup]; if (!g || g.activeIdx < 0) return null; return g.tabs[g.activeIdx]; }
export function activeFilePath() { const t = focusedTabInfo(); return t ? t.path : ''; }
export function findTabByPath(path) { for (const g of S.groups) { const idx = g.tabs.findIndex(t => t.path === path); if (idx >= 0) return { group: g, idx, tab: g.tabs[idx] }; } return null; }
export function findGroupIndexForPath(path) {
  // excludes a `missing` active tab: attachDocToPane never swapDoc's a missing
  // tab in, so S.cmPool[i] for such a group isn't actually displaying `path` —
  // callers (breakpoint verification, debug current-line) must never treat it
  // as a live pane for that path.
  for (let i = 0; i < S.groups.length; i++) { const g = S.groups[i]; if (g.activeIdx >= 0 && g.tabs[g.activeIdx] && g.tabs[g.activeIdx].path === path && !g.tabs[g.activeIdx].missing) return i; }
  return -1;
}
function visibleSlotKeys() {
  const pop = new Set(populatedGroups().map(g => g.id));
  let vis = slotOrder.filter(k => k === 'output' || pop.has(k));
  if (pop.size === 0 && !vis.includes('g0')) vis = ['g0', ...vis];
  return vis;
}
export function renderPaneGrid() {
  const grid = document.getElementById('paneGrid'); if (!grid) return;
  const vis = visibleSlotKeys();
  grid.className = 'pane-grid n' + Math.min(Math.max(vis.length, 1), 4);
  ['g0', 'g1', 'g2'].forEach(k => {
    const el = document.getElementById('pane-' + k);
    const show = vis.includes(k);
    el.style.display = show ? 'flex' : 'none';
    if (show) el.style.gridArea = AREA_LETTERS[vis.indexOf(k)];
  });
  const outEl = document.getElementById('pane-output');
  const oi = vis.indexOf('output');
  outEl.style.gridArea = oi >= 0 ? AREA_LETTERS[oi] : AREA_LETTERS[Math.min(vis.length, 3)];
}
export function attachDocToPane(groupId) {
  const gi = groupIndex(groupId); if (gi < 0) return;
  const g = S.groups[gi], cm = S.cmPool[gi];
  const missingEl = document.getElementById('paneMissing-' + groupId);
  const tab = g.activeIdx >= 0 ? g.tabs[g.activeIdx] : null;
  if (!tab) {
    cm.getWrapperElement().style.display = 'none';
    if (missingEl) missingEl.style.display = 'none';
    S.cmPathMap.set(cm, null);
    return;
  }
  if (tab.missing) {
    cm.getWrapperElement().style.display = 'none';
    if (missingEl) { missingEl.style.display = 'flex'; const p = missingEl.querySelector('.path'); if (p) p.textContent = tab.path; }
    S.cmPathMap.set(cm, null);
    return;
  }
  if (missingEl) missingEl.style.display = 'none';
  cm.getWrapperElement().style.display = 'block';
  cm.swapDoc(tab.doc);
  S.cmPathMap.set(cm, tab.path);
  renderBpGutterFor(cm, tab.path);
  ensureLspOpen(tab.path, tab.doc.getValue());
  if (cm.performLint) cm.performLint();
}
export function renderTabsForGroup(g) {
  const el = document.getElementById('paneTabs-' + g.id); if (!el) return;
  el.innerHTML = g.tabs.map((t, i) => `
    <div class="pane-tab ${i === g.activeIdx ? 'active' : ''} ${t.dirty ? 'dirty' : ''} ${t.missing ? 'missing' : ''}" role="tab" tabindex="${i === g.activeIdx ? '0' : '-1'}" aria-selected="${i === g.activeIdx}"
      draggable="true" ondragstart="tabDragStart(event,'${g.id}','${esc(t.path)}')"
      onclick="focusGroupTab('${g.id}',${i})" onkeydown="if(event.key==='Enter'||event.key===' '){event.preventDefault();focusGroupTab('${g.id}',${i});}"
      title="${t.missing ? 'File not found: ' + esc(t.path) : esc(t.path)}">
      <span class="dot"></span><span>${esc(t.path.split('/').pop())}</span>
      <button class="pane-tab-move" type="button" title="Move to another pane" aria-label="Move tab to another pane" onclick="event.stopPropagation();showMoveMenu(event,'${g.id}',${i})">⇥</button>
      <span class="x" role="button" aria-label="Close tab" onclick="closeTab('${g.id}',${i},event)">×</span>
    </div>`).join('');
}
export function renderAllTabs() { S.groups.forEach(renderTabsForGroup); }
export function focusGroupTab(groupId, idx) {
  const g = groupById(groupId); if (!g || !g.tabs[idx]) return;
  g.activeIdx = idx; S.focusedGroup = groupIndex(groupId);
  attachDocToPane(groupId);
  renderAllTabs(); renderTree();
  savePaneLayoutDebounced();
  const cm = S.cmPool[S.focusedGroup]; if (cm && !g.tabs[idx].missing) cm.focus();
}
export function showMoveMenu(e, groupId, idx) {
  e.stopPropagation();
  const g = groupById(groupId); const tab = g && g.tabs[idx]; if (!tab) return;
  const m = document.getElementById('qfPop');
  // label/order by current on-screen (slotOrder) position, not fixed group
  // identity — otherwise the numbers lie after the user drag-reorders panes.
  const items = S.groups.filter(gg => gg.id !== groupId)
    .sort((a, b) => slotOrder.indexOf(a.id) - slotOrder.indexOf(b.id))
    .map((gg, i) => `<div class="pal-item" onclick="moveTabToGroup('${groupId}','${esc(tab.path)}','${gg.id}');document.getElementById('qfPop').style.display='none';">Move to Pane ${i + 1}</div>`).join('');
  m.innerHTML = items || '<div class="pal-item">No other pane</div>';
  const r = e.target.getBoundingClientRect();
  m.style.left = Math.min(r.left, window.innerWidth - 240) + 'px'; m.style.top = (r.bottom + 4) + 'px';
  m.style.display = 'block';
  setTimeout(() => document.addEventListener('mousedown', function h(ev) { if (!ev.target.closest('#qfPop')) { m.style.display = 'none'; document.removeEventListener('mousedown', h); } }), 0);
}
export async function revealFile(path, opts) {
  opts = opts || {};
  if (path.toLowerCase().endsWith('.pdf')) { showPdf(path); return; }
  const existing = findTabByPath(path);
  if (existing) {
    if (existing.tab.missing) {
      // the file may have been recreated/renamed back into place since this
      // tab was flagged missing (e.g. via New File) — retry instead of
      // permanently trusting the stale flag.
      try {
        const r = await api('GET', `/api/projects/${S.project.id}/file?path=${encodeURIComponent(path)}`);
        existing.tab.doc = new CodeMirror.Doc(r.content, cppMode(document.getElementById('stdSelect').value));
        existing.tab.missing = false;
      } catch (_) { /* still missing */ }
    }
    existing.group.activeIdx = existing.idx; S.focusedGroup = groupIndex(existing.group.id);
    attachDocToPane(existing.group.id);
    renderAllTabs(); renderTree();
    const cm = S.cmPool[S.focusedGroup];
    if (cm && !existing.tab.missing) { if (opts.line != null) cm.setCursor({ line: opts.line, ch: opts.ch || 0 }); cm.scrollIntoView(null, 80); cm.focus(); }
    return;
  }
  let content = '', missing = false;
  try { const r = await api('GET', `/api/projects/${S.project.id}/file?path=${encodeURIComponent(path)}`); content = r.content; }
  catch (_) { missing = true; }
  const g = S.groups[S.focusedGroup];
  const tab = { path, doc: missing ? null : new CodeMirror.Doc(content, cppMode(document.getElementById('stdSelect').value)), dirty: false, missing };
  g.tabs.push(tab); g.activeIdx = g.tabs.length - 1;
  expandAncestors(path);
  attachDocToPane(g.id);
  renderPaneGrid(); renderAllTabs(); renderTree();
  savePaneLayoutDebounced();
  const cm = S.cmPool[S.focusedGroup];
  if (cm && !missing) { if (opts.line != null) cm.setCursor({ line: opts.line, ch: opts.ch || 0 }); cm.scrollIntoView(null, 80); cm.focus(); }
}
export async function openFile(path) { return revealFile(path); }
export function closeTab(groupId, idx, e) {
  if (e) e.stopPropagation();
  const g = groupById(groupId); if (!g) return;
  const t = g.tabs[idx];
  if (t && t.dirty && !confirm('Close without saving?')) return;
  g.tabs.splice(idx, 1);
  if (idx < g.activeIdx) g.activeIdx--;               // keep pointing at the same logical tab
  if (g.activeIdx >= g.tabs.length) g.activeIdx = g.tabs.length - 1;
  if (t && !t.missing && !allOpenPaths().includes(t.path)) lspDidClose(t.path);
  attachDocToPane(groupId);
  renderPaneGrid(); renderAllTabs(); renderTree();
  savePaneLayoutDebounced();
}
export function moveTabToGroup(srcId, path, destId) {
  if (srcId === destId) { const g = groupById(srcId); const idx = g && g.tabs.findIndex(t => t.path === path); if (idx >= 0) focusGroupTab(srcId, idx); return; }
  const src = groupById(srcId); if (!src) return;
  const idx = src.tabs.findIndex(t => t.path === path); if (idx < 0) return;
  const dest = groupById(destId); if (!dest) return;
  const already = dest.tabs.findIndex(t => t.path === path);
  if (already >= 0) {
    // already open in dest — drop the duplicate source tab and just focus the existing one
    src.tabs.splice(idx, 1);
    if (idx < src.activeIdx) src.activeIdx--;
    if (src.activeIdx >= src.tabs.length) src.activeIdx = src.tabs.length - 1;
    dest.activeIdx = already;
  } else {
    const [tab] = src.tabs.splice(idx, 1);
    if (idx < src.activeIdx) src.activeIdx--;
    if (src.activeIdx >= src.tabs.length) src.activeIdx = src.tabs.length - 1;
    dest.tabs.push(tab); dest.activeIdx = dest.tabs.length - 1;
  }
  S.focusedGroup = groupIndex(destId);
  attachDocToPane(srcId); attachDocToPane(destId);
  renderPaneGrid(); renderAllTabs(); renderTree();
  savePaneLayoutDebounced();
}
export function swapPanePositions(a, b) {
  if (a === b) return;
  const ia = slotOrder.indexOf(a), ib = slotOrder.indexOf(b);
  if (ia < 0 || ib < 0) return;
  [slotOrder[ia], slotOrder[ib]] = [slotOrder[ib], slotOrder[ia]];
  renderPaneGrid();
  savePaneLayoutDebounced();
}
// ── Drag and drop (native HTML5 DnD) ─────────────────────────────────────
export function tabDragStart(e, groupId, path) {
  e.stopPropagation();
  e.dataTransfer.setData('text/plain', JSON.stringify({ kind: 'tab', groupId, path }));
  e.dataTransfer.effectAllowed = 'move';
}
export function stripDragStart(e, groupId) {
  e.dataTransfer.setData('text/plain', JSON.stringify({ kind: 'pane', paneKey: groupId }));
  e.dataTransfer.effectAllowed = 'move';
}
export function outputHeaderDragStart(e) {
  e.dataTransfer.setData('text/plain', JSON.stringify({ kind: 'pane', paneKey: 'output' }));
  e.dataTransfer.effectAllowed = 'move';
}
export function paneDragOver(e) { e.preventDefault(); e.dataTransfer.dropEffect = 'move'; const p = e.currentTarget.closest('.pane'); if (p) p.classList.add('drop-target'); }
export function paneDragLeave(e) { const p = e.currentTarget.closest('.pane'); if (p) p.classList.remove('drop-target'); }
function readDragData(e) { try { return JSON.parse(e.dataTransfer.getData('text/plain')); } catch (_) { return null; } }
export function paneTabsDrop(e, paneKey) {
  e.preventDefault(); paneDragLeave(e);
  const data = readDragData(e); if (!data) return;
  if (paneKey === 'output') { if (data.kind === 'pane') swapPanePositions(data.paneKey, 'output'); return; } // Output's own strip: file drops are not a valid target
  if (data.kind === 'tab') moveTabToGroup(data.groupId, data.path, paneKey);
  else if (data.kind === 'pane') swapPanePositions(data.paneKey, paneKey);
}
export function paneBodyDrop(e, paneKey) {
  e.preventDefault(); paneDragLeave(e);
  const data = readDragData(e); if (!data) return;
  if (data.kind === 'pane') swapPanePositions(data.paneKey, paneKey);
  else if (data.kind === 'tab' && paneKey !== 'output') moveTabToGroup(data.groupId, data.path, paneKey);
}
// ── Persistence — per-project `layout` column (mirrors the `flags` pattern) ─
let paneLayoutSaveTimer = null;
function serializeLayout() {
  return { slotOrder: slotOrder.slice(), groups: S.groups.map(g => ({ id: g.id, activeIdx: g.activeIdx, tabs: g.tabs.map(t => t.path) })) };
}
export function savePaneLayoutDebounced() {
  if (!S.project) return;
  clearTimeout(paneLayoutSaveTimer);
  paneLayoutSaveTimer = setTimeout(() => {
    api('PUT', `/api/projects/${S.project.id}`, { layout: JSON.stringify(serializeLayout()) }).catch(() => { });
  }, 400);
}
export function resetGroups() {
  // cancel any pending debounced layout save from the project being left —
  // otherwise it can fire after the switch and PUT a transitional/garbled
  // layout to the newly-opened project's `layout` column.
  clearTimeout(paneLayoutSaveTimer); paneLayoutSaveTimer = null;
  S.groups = [mkGroup('g0'), mkGroup('g1'), mkGroup('g2')];
  S.focusedGroup = 0;
  slotOrder = ['g0', 'g1', 'g2', 'output'];
  for (const cm of S.cmPool) { cm.swapDoc(new CodeMirror.Doc('', cppMode(document.getElementById('stdSelect').value))); S.cmPathMap.set(cm, null); }
  ['g0', 'g1', 'g2'].forEach(k => { const el = document.getElementById('paneMissing-' + k); if (el) el.style.display = 'none'; const cm = S.cmPool[groupIndex(k)]; if (cm) cm.getWrapperElement().style.display = 'none'; });
  renderPaneGrid(); renderAllTabs();
}
// per the user's follow-up requirement: a saved path no longer on disk is NOT
// silently dropped — it's restored as a distinct "File not found" tab (closable
// / draggable like any other tab, but with no CodeMirror instance attached).
export async function restorePaneLayout() {
  resetGroups();
  const saved = S.project && S.project.layout && typeof S.project.layout === 'object' ? S.project.layout : null;
  if (!saved || !Array.isArray(saved.groups)) return;
  if (Array.isArray(saved.slotOrder) && saved.slotOrder.length) slotOrder = saved.slotOrder.slice();
  for (let gi = 0; gi < Math.min(saved.groups.length, MAX_GROUPS); gi++) {
    const sg = saved.groups[gi] || {}; const g = S.groups[gi];
    for (const path of (sg.tabs || [])) {
      let content = '', missing = false;
      try { const r = await api('GET', `/api/projects/${S.project.id}/file?path=${encodeURIComponent(path)}`); content = r.content; }
      catch (_) { missing = true; }
      g.tabs.push({ path, doc: missing ? null : new CodeMirror.Doc(content, cppMode(document.getElementById('stdSelect').value)), dirty: false, missing });
      expandAncestors(path);
    }
    g.activeIdx = g.tabs.length ? Math.min(Math.max(sg.activeIdx | 0, 0), g.tabs.length - 1) : -1;
  }
  for (const g of S.groups) if (g.activeIdx >= 0) attachDocToPane(g.id);
  renderPaneGrid(); renderAllTabs(); renderTree();
}

// resize handle drags the CSS custom property that feeds the pane grid's output row
(function () { const h = document.getElementById('resizeHandle'); let d = false; h.addEventListener('mousedown', e => { d = true; e.preventDefault(); }); document.addEventListener('mousemove', e => { if (!d) return; const hh = Math.max(80, Math.min(window.innerHeight - e.clientY - 40, window.innerHeight * .5)); document.documentElement.style.setProperty('--out-row-size', hh + 'px'); }); document.addEventListener('mouseup', () => d = false); })();

// ── Tabs: PDF preview (routed here from revealFile) ─────────────────────
async function showPdf(path) {
  try {
    const r = await fetch(`/api/projects/${S.project.id}/file/raw?path=${encodeURIComponent(path)}`);
    if (!r.ok) { setStatus('Cannot open PDF'); return; }
    const blob = await r.blob();
    const url = URL.createObjectURL(blob);
    const f = document.getElementById('pdfFrame');
    if (f.dataset.url && f.dataset.url.startsWith('blob:')) URL.revokeObjectURL(f.dataset.url);
    f.src = url; f.dataset.url = url;
    document.getElementById('pdfTitle').textContent = path;
    document.getElementById('pdfModal').style.display = 'flex';
  } catch (e) { setStatus('PDF preview failed'); }
}
export function closePdf() {
  document.getElementById('pdfModal').style.display = 'none';
  const f = document.getElementById('pdfFrame');
  if (f.dataset.url && f.dataset.url.startsWith('blob:')) URL.revokeObjectURL(f.dataset.url);
  f.src = 'about:blank'; delete f.dataset.url;
}

// ── Editor events (wired once per pool instance at creation time; called
// from the init loop above — function declarations are hoisted, so the
// forward reference there resolves fine) ──────────────────────────────────
function wireCmInstance(cm, gi) {
  cm.on('change', () => {
    const path = S.cmPathMap.get(cm); if (!path) return;
    const found = findTabByPath(path); if (!found) return;
    found.tab.dirty = true; renderAllTabs(); scheduleAutoSave(path);
    lspDidChangePath(path, cm.getValue());
  });
  cm.on('cursorActivity', () => { if (S.focusedGroup !== gi) return; const c = cm.getCursor(); document.getElementById('sbPos').textContent = 'Ln ' + (c.line + 1) + ', Col ' + (c.ch + 1); });
  cm.on('mousemove', e => onEditorMouseMove(cm, e));
  cm.on('mousedown', (cmi, e) => { S.focusedGroup = gi; if (e.ctrlKey || e.metaKey) { e.preventDefault(); setTimeout(() => gotoDefinition(cm), 0); } });
  cm.on('focus', () => { S.focusedGroup = gi; });
  cm.on('gutterClick', (cmi, line, gutter) => { if (gutter === 'breakpoints') { const path = S.cmPathMap.get(cm); toggleBreakpoint(cm, path, line); } });
  cm.on('inputRead', (cmi, ch) => { if (!S.lsp.ready || ch.origin !== '+input') return; const c = ch.text[0]; if (c === '.' || c === '>' || c === ':') setTimeout(() => cm.showHint({ hint: clangHint, completeSingle: false }), 0); if (c === '(' || c === ',') setTimeout(() => showSignature(cm), 0); });
  cm.on('scroll', () => hideHoverPopup());
}

// exposed for inline HTML handlers
Object.assign(window, {
  focusGroupTab, showMoveMenu, closeTab, moveTabToGroup, revealFile,
  tabDragStart, stripDragStart, outputHeaderDragStart, paneDragOver, paneDragLeave, paneTabsDrop, paneBodyDrop,
  closePdf,
});
