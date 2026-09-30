// ── Bootstrap: command palette, global keybindings, icon hydration, the
// external-change focus-refresh poll, the backend/clangd status dots, and
// the final init sequence. This is the only module index.html loads
// directly — everything else is reached through it (or through the
// modules it imports).
import { S } from './state.js';
import { api, icon, esc, setDot, flashErr } from './api.js';
import { allOpenTabs, resetGroups } from './editor-panes.js';
import { refreshLint, updateDiagCounts, lspConnect, gotoDefinition, quickFixMenu, debugToggle } from './lsp-debug.js';
import {
  THEMES, FONT_SIZES, INDENTS, applyTheme, applyFontSize, applyIndent, applyStd,
  buildThemeSelect, buildFontSelect, buildIndentSelect,
  loadProjects, openProject, createProject, onTitleChange,
  loadTrash, renderTrash, loadTreeRefresh, renderTree, updateProjectState,
  runCode, formatCode, saveActiveFile, newFile, newFolder, runTests,
  showSubmit, showVscode, toggleDocs, switchOutTab,
} from './projects.js';
import { showAdmin } from './admin-marking.js';

// ── Command palette (Ctrl+Shift+P) ─────────────────────────────────────
const PALETTE_CMDS = [
  { label: 'Run program', hint: 'Ctrl+Enter', fn: () => runCode() },
  { label: 'Rebuild and run', hint: 'Shift+Ctrl+Enter', fn: () => runCode(true) },
  { label: 'Debug with lldb', hint: 'F5', fn: debugToggle },
  { label: 'Format file', hint: '', fn: formatCode },
  { label: 'Save file', hint: 'Ctrl+S', fn: saveActiveFile },
  { label: 'New project', hint: '', fn: createProject },
  { label: 'New file', hint: '', fn: () => newFile() },
  { label: 'New folder', hint: '', fn: () => newFolder() },
  { label: 'Run tests', hint: '', fn: () => { switchOutTab('tests'); runTests(); } },
  { label: 'Submit project', hint: '', fn: showSubmit },
  { label: 'Open in VS Code', hint: '', fn: showVscode },
  { label: 'Toggle cppreference docs', hint: '', fn: toggleDocs },
  { label: 'Admin panel', hint: '', fn: showAdmin },
  { label: 'Output panel: Input (stdin)', hint: '', fn: () => switchOutTab('input') },
  { label: 'Output panel: Compile Log', hint: '', fn: () => switchOutTab('compile') },
  { label: 'Go to definition', hint: 'F12 / Ctrl+Click', fn: gotoDefinition },
  { label: 'Quick fix…', hint: 'Ctrl+.', fn: quickFixMenu },
  { label: 'Output panel: Problems', hint: '', fn: () => switchOutTab('problems') },
];
let palIdx = 0;
function openPalette() {
  document.getElementById('paletteInput').value = '';
  document.getElementById('paletteModal').classList.add('show');
  palIdx = 0; renderPalette('');
  setTimeout(() => document.getElementById('paletteInput').focus(), 0);
}
function hidePalette() { document.getElementById('paletteModal').classList.remove('show'); }
function paletteMatches(q) {
  q = q.toLowerCase();
  return PALETTE_CMDS.filter(c => c.label.toLowerCase().includes(q));
}
function renderPalette(q) {
  const ms = paletteMatches(q);
  if (palIdx >= ms.length) palIdx = Math.max(0, ms.length - 1);
  document.getElementById('paletteList').innerHTML = ms.map((c, i) =>
    `<div class="pal-item ${i === palIdx ? 'active' : ''}" onclick="paletteRun(${PALETTE_CMDS.indexOf(c)})">${esc(c.label)}${c.hint ? `<span style="margin-left:auto;color:var(--dim);font-size:11px;">${esc(c.hint)}</span>` : ''}</div>`).join('')
    || '<div style="padding:10px;color:var(--dim);">No matching command</div>';
}
function paletteRun(i) { hidePalette(); PALETTE_CMDS[i].fn(); }
function paletteKeys(e) {
  const ms = paletteMatches(e.target.value);
  if (e.key === 'ArrowDown') { e.preventDefault(); palIdx = Math.min(palIdx + 1, ms.length - 1); renderPalette(e.target.value); }
  else if (e.key === 'ArrowUp') { e.preventDefault(); palIdx = Math.max(palIdx - 1, 0); renderPalette(e.target.value); }
  else if (e.key === 'Enter') { e.preventDefault(); if (ms[palIdx]) paletteRun(PALETTE_CMDS.indexOf(ms[palIdx])); }
  else if (e.key === 'Escape') { hidePalette(); }
  else setTimeout(() => renderPalette(e.target.value), 0);
}

// ── inline SVG icon set: hydrate every [data-icon] element in the static markup ─
(function hydrateIcons() { document.querySelectorAll('[data-icon]').forEach(el => { el.insertAdjacentHTML('afterbegin', icon(el.dataset.icon) + ' '); }); })();

document.getElementById('titleInput').addEventListener('change', onTitleChange);
window.addEventListener('keydown', e => { if ((e.ctrlKey || e.metaKey) && e.key === 'f') e.preventDefault(); if (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === 'p') { e.preventDefault(); openPalette(); } });

// ── External-change refresh (VS Code / other editors on the same folder) ──
// ponytail: focus-poll, not an fs watcher; swap for notify+WS push if concurrent editing gets tight
let lastFocus = Date.now();
window.addEventListener('focus', () => {
  if (!S.project || Date.now() - lastFocus < 800) return;
  lastFocus = Date.now();
  (async () => {
    try {
      S.tree = await api('GET', `/api/projects/${S.project.id}/tree`); renderTree();
      // re-read every open (non-missing, non-dirty) tab across every group —
      // not just the focused one
      for (const t of allOpenTabs()) {
        if (t.dirty || t.missing) continue;
        const r = await api('GET', `/api/projects/${S.project.id}/file?path=${encodeURIComponent(t.path)}`);
        if (t.doc.getValue() !== r.content) t.doc.setValue(r.content);
      }
      refreshLint();
    } catch (_) { }
  })();
});

// ── Status bar ───────────────────────────────────────────────────────────
// Two backends are selectable and neither falls back to the other, so the dot
// has to say something about *the one this project will actually use* - a green
// dot for a ready WASM toolchain is misleading on a project set to Native. The
// fetch stays on a 15s interval; re-rendering from the cached response is what
// makes switching project or backend feel immediate without extra requests.
let backendStatus = null;

function selectedBackend() {
  return ((S.project && S.project.flags) || {}).backend === 'native' ? 'native' : 'wasm';
}

function renderBackendDot() {
  const label = document.getElementById('sbBackendLabel');
  const dot = document.getElementById('sbBackend');
  if (!dot) return;
  const sel = selectedBackend();
  if (label) label.textContent = sel === 'native' ? 'native' : 'wasm';
  if (!backendStatus) { setDot('sbBackend', 'bad'); return; }
  const wasm = backendStatus.wasi || {};
  const native = backendStatus.native || {};
  const ready = sel === 'native' ? !!native.ready : !!wasm.ready;
  setDot('sbBackend', ready ? 'ok' : 'bad');
  const host = dot.parentElement;
  if (host) {
    host.title = `Backend for this project: ${sel === 'native' ? 'Native (host clang++)' : 'WASM (wasm32-wasip1)'}\n`
      + `WASM: ${wasm.message || (wasm.ready ? 'ready' : 'not ready')}\n`
      + `Native: ${native.message || (native.ready ? 'ready' : 'not ready')}`;
  }
  // Offering "Native" on a machine without clang++ is a trap; say so in the
  // option itself rather than disabling it (a project already set to Native
  // must stay selectable so its error explains itself).
  const opt = document.querySelector('#tcBackend option[value="native"]');
  if (opt) opt.textContent = native.ready ? 'Native' : 'Native (clang++ not found)';
}

async function pollBackend() {
  try { backendStatus = await api('GET', '/api/sandbox/status'); }
  catch (_) { backendStatus = null; }
  renderBackendDot();
}
pollBackend(); setInterval(pollBackend, 15000);
setInterval(renderBackendDot, 2000);
// The inline onchange="saveFlags()" has already written S.project.flags by the
// time this fires, so the dot can be re-rendered straight away.
document.getElementById('tcBackend')?.addEventListener('change', renderBackendDot);
updateDiagCounts();

// ── Tablist keyboard nav (editor tabs + output tabs) ─────────────────────
function tablistKeys(rootSel, activate) {
  const root = document.querySelector(rootSel); if (!root) return;
  root.addEventListener('keydown', e => {
    if (e.key !== 'ArrowRight' && e.key !== 'ArrowLeft') return;
    const tabs = [...root.querySelectorAll('[role=tab]')]; if (!tabs.length) return;
    const i = tabs.indexOf(document.activeElement); if (i < 0) return;
    e.preventDefault();
    const n = tabs[(i + (e.key === 'ArrowRight' ? 1 : tabs.length - 1)) % tabs.length];
    n.focus(); activate(n);
  });
}
tablistKeys('#paneTabs-g0', t => t.click());
tablistKeys('#paneTabs-g1', t => t.click());
tablistKeys('#paneTabs-g2', t => t.click());
tablistKeys('.out-tabs', t => switchOutTab(t.dataset.tab));

// ── Init ─────────────────────────────────────────────────────────────────
resetGroups();
buildThemeSelect();
buildFontSelect();
buildIndentSelect();
lspConnect();
(async function init() {
  try {
    const s = await api('GET', '/api/settings');
    if (THEMES[s.theme]) { document.getElementById('themeSelect').value = s.theme; applyTheme(s.theme); }
    if (FONT_SIZES.includes(s.font_size)) { document.getElementById('fontSelect').value = s.font_size; applyFontSize(s.font_size); }
    if (INDENTS.includes(s.indent)) { document.getElementById('indentSelect').value = s.indent; applyIndent(s.indent); document.getElementById('sbIndent').textContent = 'Spaces: ' + s.indent; }
    const stdSel = document.getElementById('stdSelect');
    if ([...stdSel.options].some(o => o.value === s.std)) { stdSel.value = s.std; applyStd(s.std); }
    const projects = await loadProjects();
    if (s.last_project_id && projects.some(p => p.id === s.last_project_id)) await openProject(s.last_project_id);
  } catch (e) { flashErr(e); }
  updateProjectState();
})();
loadTrash().catch(() => { });
renderTrash();
loadTreeRefresh();
updateProjectState();

// exposed for inline HTML handlers
Object.assign(window, { hidePalette, paletteKeys, paletteRun });
