// ── Projects: list/CRUD, trash, file tree, file tabs save/format, editor
// settings (theme/font/indent/std/toolchain flags), run/rebuild, tests,
// the Output pane's tabs (output/compile/input/tests/problems), the
// cppreference docs side panel, the VS Code (Remote-SSH) modal, and the
// Submit modal.
import { S } from './state.js';
import { api, esc, icon, ask, toast, setStatus, flashErr, timeAgo } from './api.js';
import {
  findTabByPath, focusedCm, activeFilePath, allOpenTabs, allOpenPaths, populatedGroups,
  revealFile, resetGroups, restorePaneLayout, renderAllTabs, savePaneLayoutDebounced, closeTab, cppMode,
  attachDocToPane,
} from './editor-panes.js';
import { resetDebugState, lspConnect, lspDidClose, renameDebugBreakpoints } from './lsp-debug.js';

// ── Themes ───────────────────────────────────────────────────────────────
export const THEMES = {
  'material-ocean': { kind: 'dark', cm: 'material-ocean', vars: { '--bg': '#0f111a', '--surface': '#1a1d2e', '--surface2': '#252840', '--border': '#2d3154', '--text': '#cdd6f4', '--dim': '#8f93a8', '--accent': '#89b4fa', '--green': '#a6e3a1', '--red': '#f38ba8', '--yellow': '#f9e2af' } },
  'dracula': { kind: 'dark', cm: 'dracula', vars: { '--bg': '#282a36', '--surface': '#21222c', '--surface2': '#343746', '--border': '#44475a', '--text': '#f8f8f2', '--dim': '#8b93b8', '--accent': '#bd93f9', '--green': '#50fa7b', '--red': '#ff5555', '--yellow': '#f1fa8c' } },
  'blackboard': { kind: 'dark', cm: 'blackboard', vars: { '--bg': '#0c1021', '--surface': '#15192b', '--surface2': '#1f2438', '--border': '#2a3050', '--text': '#e8e8e8', '--dim': '#8a95b5', '--accent': '#7aa2ff', '--green': '#7fd884', '--red': '#ff6b6b', '--yellow': '#ffd866' } },
  'eclipse': { kind: 'light', cm: 'eclipse', vars: { '--bg': '#f7f7f7', '--surface': '#efefef', '--surface2': '#e2e2e2', '--border': '#cfcfcf', '--text': '#1b1b1b', '--dim': '#777', '--accent': '#1a73e8', '--green': '#1e8e3e', '--red': '#d93025', '--yellow': '#b26a00' } },
  'idea': { kind: 'light', cm: 'idea', vars: { '--bg': '#ffffff', '--surface': '#f4f4f6', '--surface2': '#e6e6ea', '--border': '#d4d4d8', '--text': '#1e1e1e', '--dim': '#808088', '--accent': '#2a7de1', '--green': '#21a766', '--red': '#d23e3e', '--yellow': '#c47f00' } },
  'default': { kind: 'light', cm: 'default', vars: { '--bg': '#ffffff', '--surface': '#f5f5f5', '--surface2': '#e8e8e8', '--border': '#d0d0d0', '--text': '#222', '--dim': '#888', '--accent': '#0a66c2', '--green': '#097d3a', '--red': '#c0271c', '--yellow': '#9a6a00' } },
};

// ── Projects ─────────────────────────────────────────────────────────────
let projects = [];
export async function loadProjects() { projects = await api('GET', '/api/projects'); renderProjects(); return projects; }
// single source of truth: `S.project`. Drives disabled buttons + empty state.
export function updateProjectState() {
  const has = !!S.project;
  document.getElementById('titleInput').disabled = !has;
  document.getElementById('titleInput').placeholder = has ? 'Project name…' : 'No project open';
  for (const id of ['formatBtn', 'saveBtn', 'runBtn', 'rebuildBtn', 'dbgBtn', 'submitBtn', 'tcBtn']) {
    const b = document.getElementById(id); if (b) b.disabled = !has;
  }
  document.getElementById('emptyState').classList.toggle('show', !has);
  if (!has) { document.getElementById('treeTitle').textContent = 'Files'; document.getElementById('treeBody').innerHTML = '<div class="tree-empty">Open a project</div>'; }
  loadFlagsIntoUI();
}
export function renderProjects() {
  const el = document.getElementById('projList');
  if (!projects.length) { el.innerHTML = '<div style="padding:24px 12px;color:var(--dim);font-size:12px;text-align:center">No projects yet.<br>Click + New Project</div>'; return; }
  el.innerHTML = projects.map(p => `
    <div class="proj-item ${S.project && p.id === S.project.id ? 'active' : ''}" onclick="openProject('${p.id}')">
      <div class="t"><span>${p.local_path ? icon('folder', 11) + ' ' : ''}${esc(p.title)}</span>
        <span style="display:flex;gap:2px;">
          <button class="del" aria-label="Rename project" title="Rename" onclick="event.stopPropagation();renameProject('${p.id}')">✎</button>
          <button class="del" aria-label="Delete project" title="Move to Trash" onclick="event.stopPropagation();deleteProject('${p.id}')">✕</button>
        </span></div>
      <div class="std">${p.cpp_standard} · ${timeAgo(p.updated_at)}</div>
    </div>`).join('');
}
export async function createProject() {
  try {
    const title = await ask('New Project', 'Project name', 'Untitled'); if (!title) return;
    const localPath = await ask('New Project', 'Local path (optional — for Google Drive sync)', ''); if (localPath === null) return;
    const p = await api('POST', '/api/projects', { title: title || 'Untitled', cpp_standard: document.getElementById('stdSelect').value, local_path: localPath || null });
    await loadProjects(); await openProject(p.id);
  } catch (e) { flashErr(e); }
}
export async function openProject(id) {
  try {
    await saveAllDirty();
    flushStdin();
    S.project = await api('GET', `/api/projects/${id}`);
    document.getElementById('titleInput').value = S.project.title;
    document.getElementById('stdSelect').value = S.project.cpp_standard;
    applyStd(S.project.cpp_standard);
    document.getElementById('treeTitle').textContent = 'Files · ' + S.project.title;
    if (S.project.local_path) document.getElementById('treeTitle').textContent += ' · ' + S.project.local_path;
    resetDebugState();
    await loadTree();
    await restorePaneLayout();
    // open main.cpp if present and nothing was restored from a saved layout
    if (populatedGroups().length === 0) {
      const mainPath = findFile(S.tree, 'main.cpp');
      if (mainPath) await revealFile(mainPath); else setStatus('');
    }
    // fresh clangd session/workspace for this project — the LSP connection is
    // otherwise scoped to whichever project was active when it first opened
    // and never re-synced, so switching projects would silently leave clangd
    // pointed at the previous project's files.
    lspConnect();
    renderProjects();
    updateProjectState();
    api('PUT', '/api/settings', { last_project_id: id }).catch(() => { });
  } catch (e) { flashErr(e); }
}
export async function renameProject(id) {
  const p = projects.find(x => x.id === id); if (!p) return;
  const t = await ask('Rename Project', 'Project name', p.title); if (!t || t === p.title) return;
  try {
    await api('PUT', `/api/projects/${id}`, { title: t });
    p.title = t;
    if (S.project && S.project.id === id) { S.project.title = t; document.getElementById('titleInput').value = t; document.getElementById('treeTitle').textContent = 'Files · ' + t; }
    renderProjects();
  } catch (e) { flashErr(e); }
}
export async function deleteProject(id) {
  const p = projects.find(x => x.id === id);
  if (!confirm(`Move "${p ? p.title : id}" to Trash? You can restore it from the sidebar.`)) return;
  try {
    await api('DELETE', `/api/projects/${id}`);
    if (S.project && S.project.id === id) { S.project = null; resetGroups(); loadTreeRefresh(); updateProjectState(); }
    await loadProjects(); await loadTrash();
    toast(`"${p ? p.title : id}" moved to Trash`, 'info', {
      label: 'Undo', fn: async () => {
        try { await api('POST', `/api/projects/${id}/restore`); await loadProjects(); await loadTrash(); }
        catch (e) { flashErr(e); }
      }
    });
  } catch (e) { flashErr(e); }
}

// ── Trash ────────────────────────────────────────────────────────────────
let trash = [];
export async function loadTrash() { try { trash = await api('GET', '/api/trash'); renderTrash(); } catch (e) { flashErr(e); } }
export function renderTrash() {
  document.getElementById('trashCount').textContent = trash.length ? ('(' + trash.length + ')') : '';
  const el = document.getElementById('trashBody');
  if (!trash.length) { el.innerHTML = '<div class="trash-item" style="color:var(--dim)">Trash is empty</div>'; return; }
  el.innerHTML = trash.map(p => `
    <div class="trash-item">
      <div class="t" onclick="openProject('${p.id}')" title="Click to review files"><span>${esc(p.title)}</span></div>
      <div class="meta">${p.cpp_standard} · deleted ${timeAgo(p.deleted_at)}</div>
      <div class="acts" style="margin-top:4px;">
        <button class="mini-btn" data-icon="restore" onclick="restoreProject('${p.id}')">Restore</button>
        <button class="mini-btn del" data-icon="trash" onclick="purgeProject('${p.id}')">Delete forever</button>
      </div>
    </div>`).join('');
}
export function toggleTrash() {
  const b = document.getElementById('trashBody'); const open = b.classList.toggle('open');
  document.getElementById('trashCaret').textContent = open ? '▾' : '▸';
  if (open) loadTrash();
}
export async function restoreProject(id) {
  try { await api('POST', `/api/projects/${id}/restore`); await loadProjects(); await loadTrash(); setStatus('Restored'); }
  catch (e) { flashErr(e); }
}
export async function purgeProject(id) {
  const p = trash.find(x => x.id === id);
  if (!confirm('Permanently delete "' + (p ? p.title : id) + '"? This cannot be undone.')) return;
  try { await api('DELETE', `/api/projects/${id}/purge`); if (S.project && S.project.id === id) { S.project = null; resetGroups(); loadTreeRefresh(); } await loadProjects(); await loadTrash(); setStatus('Deleted forever'); }
  catch (e) { flashErr(e); }
}
function findFile(node, name) { if (node.type === 'file' && node.name === name) return node.path; if (node.children) for (const c of node.children) { const r = findFile(c, name); if (r) return r; } return null; }

// ── File tree ────────────────────────────────────────────────────────────
let expandedDirs = new Set();
export async function loadTree() { S.tree = await api('GET', `/api/projects/${S.project.id}/tree`); renderTree(); }
export function loadTreeRefresh() { if (S.project) { api('GET', `/api/projects/${S.project.id}/tree`).then(t => { S.tree = t; renderTree(); }); } else { S.tree = null; document.getElementById('treeBody').innerHTML = '<div class="tree-empty">Open a project</div>'; } }
export function renderTree() {
  const el = document.getElementById('treeBody');
  if (!S.project) { el.innerHTML = '<div class="tree-empty">Open a project</div>'; return; }
  el.innerHTML = renderNodes(S.tree.children || []);
}
function renderNodes(nodes, depth = 0) {
  return nodes.map(n => {
    const pad = 'padding-left:' + (8 + depth * 12) + 'px;';
    if (n.type === 'dir') {
      const open = expandedDirs.has(n.path);
      const kids = n.children && n.children.length ? renderNodes(n.children, depth + 1) : '<div class="tree-empty" style="padding:2px 0 2px ' + (20 + depth * 12) + 'px">(empty)</div>';
      return `<div class="tree-row" style="${pad}" data-path="${esc(n.path)}" onclick="toggleFolder(this)">
        <span class="ico">${open ? icon('folderopen', 13) : icon('folder', 13)}</span><span>${esc(n.name)}</span>
        <span class="acts"><button aria-label="New file here" onclick="event.stopPropagation();newFile('${esc(n.path)}')" title="New file">${icon('fileplus', 12)}</button><button aria-label="Rename" onclick="event.stopPropagation();renameNode('${esc(n.path)}')" title="Rename">${icon('pencil', 12)}</button><button aria-label="Delete" onclick="event.stopPropagation();deleteNode('${esc(n.path)}')" title="Delete">${icon('trash', 12)}</button></span>
      </div><div class="folder-kids" style="display:${open ? 'block' : 'none'}">${kids}</div>`;
    }
    const active = allOpenPaths().includes(n.path) ? 'active' : '';
    return `<div class="tree-row ${active}" style="${pad}" onclick="revealFile('${esc(n.path)}')">
      <span class="ico">${icon('file', 13)}</span><span>${esc(n.name)}</span>
      <span class="acts"><button aria-label="Rename" onclick="event.stopPropagation();renameNode('${esc(n.path)}')" title="Rename">${icon('pencil', 12)}</button><button aria-label="Delete" onclick="event.stopPropagation();deleteNode('${esc(n.path)}')" title="Delete">${icon('trash', 12)}</button></span>
    </div>`;
  }).join('');
}
export function toggleFolder(row) {
  const p = row.dataset.path; const kids = row.nextElementSibling; if (!kids || !kids.classList.contains('folder-kids')) return;
  const open = kids.style.display !== 'block';
  kids.style.display = open ? 'block' : 'none';
  if (open) expandedDirs.add(p); else expandedDirs.delete(p);
  const ico = row.querySelector('.ico'); if (ico) ico.innerHTML = open ? icon('folderopen', 13) : icon('folder', 13);
}
export function expandAncestors(path) {
  let parts = (path || '').split('/'); parts.pop();
  let cur = '';
  for (const p of parts) { cur = cur ? cur + '/' + p : p; expandedDirs.add(cur); }
}
export function toggleTree() {
  const p = document.getElementById('treePanel');
  p.classList.toggle('collapsed');
  document.getElementById('expandTree').style.display = p.classList.contains('collapsed') ? 'block' : 'none';
}

// ── cppreference docs panel (restricted to https://www.cppreference.com/*) ──
export function toggleDocs() {
  const p = document.getElementById('docsPanel');
  p.classList.toggle('open');
  const f = document.getElementById('docsFrame');
  if (p.classList.contains('open') && !f.src) docsHome();
}
function docsAllowed(url) {
  try { const u = new URL(url); return u.protocol === 'https:' && u.hostname === 'www.cppreference.com'; }
  catch (e) { return false; }
}
function docsLoad(url) {
  const msg = document.getElementById('docsMsg'), f = document.getElementById('docsFrame');
  if (!docsAllowed(url)) { msg.style.display = 'block'; msg.textContent = 'Blocked: only https://www.cppreference.com/* can be displayed here.'; return; }
  msg.style.display = 'none';
  f.src = url;
  document.getElementById('docsUrl').value = url;
}
export function docsHome() { docsLoad('https://www.cppreference.com/'); }
export function docsGo() {
  const v = document.getElementById('docsUrl').value.trim();
  if (!v) return;
  if (/^https?:\/\//i.test(v)) docsLoad(v);   // full URL — validated by docsAllowed
  else docsLoad('https://www.cppreference.com/mwiki/index.php?search=' + encodeURIComponent(v));
}
export async function newFile(dirPath) {
  if (!S.project) { setStatus('Open or create a project first'); return; }
  try {
    const base = dirPath ? dirPath + '/' : '';
    const name = await ask('New File', 'File path (relative to project)', base + 'new.cpp', '.cpp .h .hpp — created empty and opened'); if (!name) return;
    await api('PUT', `/api/projects/${S.project.id}/file`, { path: name, content: '', is_dir: false });
    await loadTree(); await revealFile(name);
  } catch (e) { flashErr(e); }
}
export async function newFolder(dirPath) {
  if (!S.project) { setStatus('Open or create a project first'); return; }
  try {
    const base = dirPath ? dirPath + '/' : '';
    const name = await ask('New Folder', 'Folder path (relative to project)', base + 'folder'); if (!name) return;
    await api('PUT', `/api/projects/${S.project.id}/file`, { path: name, is_dir: true });
    await loadTree();
  } catch (e) { flashErr(e); }
}
export async function renameNode(path) {
  const np = await ask('Rename / Move', 'New path', path); if (!np || np === path) return;
  await api('POST', `/api/projects/${S.project.id}/file/move`, { old_path: path, new_path: np });
  // update open tab path if open (in every group), and resync everything else
  // that's keyed by the OLD path — cmPathMap (via attachDocToPane), the LSP
  // open-doc map, and persisted breakpoints — none of which update themselves
  // just because `tab.path` changed.
  lspDidClose(path);
  renameDebugBreakpoints(path, np);
  for (const g of S.groups) {
    const t = g.tabs.find(t => t.path === path);
    if (t) { t.path = np; if (g.activeIdx >= 0 && g.tabs[g.activeIdx] === t) attachDocToPane(g.id); }
  }
  await loadTree(); renderAllTabs(); savePaneLayoutDebounced();
}
export async function deleteNode(path) {
  if (!confirm('Delete "' + path + '"?')) return;
  await api('DELETE', `/api/projects/${S.project.id}/file?path=${encodeURIComponent(path)}`);
  // close tab if open (in whichever group has it)
  for (const g of S.groups) {
    const i = g.tabs.findIndex(t => t.path === path);
    if (i >= 0) closeTab(g.id, i);
  }
  await loadTree();
}

// ── Tabs: save ───────────────────────────────────────────────────────────
async function saveFileByPath(path) {
  const found = findTabByPath(path); if (!found || !S.project) return;
  const tab = found.tab; if (tab.missing) return;
  try {
    const code = tab.doc.getValue();
    const cpp = isCppFile(path);
    const indent = parseInt(document.getElementById('indentSelect').value) || 2;
    if (code.trim() && cpp) {
      const f = await api('POST', '/api/format', { code, indent });
      if (f.formatted && f.formatted !== code) tab.doc.setValue(f.formatted);
    }
    await api('PUT', `/api/projects/${S.project.id}/file`, { path, content: tab.doc.getValue() });
    tab.dirty = false; renderAllTabs(); setStatus(cpp ? 'Saved & formatted ✓' : 'Saved ✓');
    const n = new Date(); document.getElementById('sbSaved').textContent = 'Saved · ' + String(n.getHours()).padStart(2, '0') + ':' + String(n.getMinutes()).padStart(2, '0');
  } catch (e) { flashErr(e); }
}
export async function saveActiveFile(cm) {
  cm = cm || focusedCm();
  const path = S.cmPathMap.get(cm) || activeFilePath();
  if (!path || !S.project) { setStatus('Nothing to save'); return; }
  await saveFileByPath(path);
}
export async function saveAllDirty() {
  const dirty = allOpenTabs().filter(t => t.dirty && !t.missing);
  for (const t of dirty) await saveFileByPath(t.path);
}
let autoSaveTimers = new Map();
export function scheduleAutoSave(path) {
  if (!S.project) return;
  clearTimeout(autoSaveTimers.get(path));
  autoSaveTimers.set(path, setTimeout(() => saveFileByPath(path), 1200));
}

// ── Title / std ──────────────────────────────────────────────────────────
export async function onTitleChange() { if (!S.project) return; S.project.title = document.getElementById('titleInput').value || 'Untitled'; await api('PUT', `/api/projects/${S.project.id}`, { title: S.project.title }); document.getElementById('treeTitle').textContent = 'Files · ' + S.project.title; renderProjects(); }
export async function onStdChange() {
  applyStd(document.getElementById('stdSelect').value);
  for (const cm of S.cmPool) if (cm.performLint) cm.performLint();
  if (S.project) { await api('PUT', `/api/projects/${S.project.id}`, { cpp_standard: document.getElementById('stdSelect').value }); renderProjects(); }
}
export function applyStd(std) {
  const mode = cppMode(std);
  // Each open file's Doc stores its own `modeOption`, restored verbatim by
  // CodeMirror on swapDoc — cm.setOption('mode', ...) alone only affects
  // whichever Doc is *currently* attached, leaving background tabs' Docs
  // frozen on the old standard until closed and reopened. Update every open
  // Doc directly (mirroring what CM5's own mode-option setter does for the
  // attached Doc) so a background tab picks up the new standard too.
  for (const t of allOpenTabs()) if (t.doc) t.doc.modeOption = mode;
  for (const cm of S.cmPool) cm.setOption('mode', mode);
  document.getElementById('clangVer').textContent = 'clangd'; document.getElementById('sbStd').textContent = std;
}

export function toggleGear(e) { e.stopPropagation(); document.getElementById('settingsPop').classList.toggle('open'); }
document.addEventListener('click', e => { const p = document.getElementById('settingsPop'); if (p && p.classList.contains('open') && !e.target.closest('.gear-wrap')) p.classList.remove('open'); });

// ── Toolchain flags (per project → Makefile) ──────────────────────────
export function toggleTcPop(e) { e.stopPropagation(); document.getElementById('tcPop').classList.toggle('open'); }
document.addEventListener('click', e => { const p = document.getElementById('tcPop'); if (p && p.classList.contains('open') && !e.target.closest('#tcPop') && !e.target.closest('#tcBtn')) p.classList.remove('open'); });
function loadFlagsIntoUI() {
  const f = (S.project && S.project.flags) || {};
  document.getElementById('tcWerror').checked = !!f.werror;
  document.getElementById('tcSanitize').checked = !!f.sanitize;
  document.getElementById('tcOpt').value = f.opt === '0' ? '0' : '2';
  // 'wasm' | 'native' are the only selectable backends; legacy 'auto'/'podman'
  // values (and anything unknown) show as wasm, matching Backend::from_flags.
  document.getElementById('tcBackend').value = f.backend === 'native' ? 'native' : 'wasm';
  // '30' | '60' | 'inf'; anything else (including absent, on a project saved
  // before this existed) shows as the 60s default, matching sandbox::parse_run_timeout.
  document.getElementById('tcTimeout').value = ['30', 'inf'].includes(f.timeout) ? f.timeout : '60';
}
let flagsSaveTimer = null;
export function saveFlags() {
  if (!S.project) return;
  const prevBackend = (S.project.flags || {}).backend;
  const flags = {
    werror: document.getElementById('tcWerror').checked,
    sanitize: document.getElementById('tcSanitize').checked,
    opt: document.getElementById('tcOpt').value,
    backend: document.getElementById('tcBackend').value,
    timeout: document.getElementById('tcTimeout').value,
  };
  S.project.flags = flags;
  // Native runs the program as an ordinary host process - no container, no
  // memory cap. Worth saying once, when they opt in, rather than nagging.
  if (flags.backend === 'native' && prevBackend !== 'native') {
    setStatus('Native backend: builds with host clang++ and runs unsandboxed. Supports threads, <execution> and sanitizers.');
  }
  clearTimeout(flagsSaveTimer);
  flagsSaveTimer = setTimeout(() => {
    api('PUT', `/api/projects/${S.project.id}`, { flags: JSON.stringify(flags) })
      .then(() => setStatus('Toolchain saved (rebuild to apply: Shift+Ctrl+Enter)'))
      .catch(e => flashErr(e));
  }, 250);
}

// Regenerating a Makefile is deliberately never automatic — the backend
// (create/update project, and make_and_run's own regen check) only ever
// (re)writes one when it's absent or still carries CPPBox's own template
// marker, so a real, user-authored Makefile (e.g. an imported course
// exercise) is left alone. This button is the one explicit, user-triggered
// way to force-overwrite it — always confirm first, with an extra-blunt
// warning when a Makefile is already there to overwrite.
export async function regenerateMakefile() {
  if (!S.project) return;
  let exists = true;
  try { await api('GET', `/api/projects/${S.project.id}/file?path=Makefile`); }
  catch (_) { exists = false; }
  const msg = exists
    ? '⚠ This project already has a Makefile. Regenerating will PERMANENTLY OVERWRITE it with a CPPBox-generated one, discarding anything custom in it (unless the project is tracked in git). Continue?'
    : 'No Makefile exists yet in this project. Create one from the current C++ standard and toolchain flags?';
  if (!confirm(msg)) return;
  try {
    await api('POST', `/api/projects/${S.project.id}/makefile/regenerate`);
    setStatus('Makefile regenerated ✓');
    await loadTree();
  } catch (e) { flashErr(e); }
}

// ── Themes ───────────────────────────────────────────────────────────────
export function buildThemeSelect() {
  const sel = document.getElementById('themeSelect');
  const og = Object.assign(document.createElement('optgroup'), { label: 'Dark' });
  const lg = Object.assign(document.createElement('optgroup'), { label: 'Light' });
  for (const [k, v] of Object.entries(THEMES)) { const o = new Option(k, k); (v.kind === 'dark' ? og : lg).appendChild(o); }
  sel.appendChild(og); sel.appendChild(lg);
  sel.value = 'material-ocean';
  applyTheme(sel.value);
}
export function applyTheme(id) {
  const t = THEMES[id]; if (!t) return;
  const r = document.documentElement.style;
  for (const [k, v] of Object.entries(t.vars)) r.setProperty(k, v);
  for (const cm of S.cmPool) cm.setOption('theme', t.cm);
}

// ── Font size ────────────────────────────────────────────────────────────
export const FONT_SIZES = [11, 12, 13, 14, 15, 16, 18, 20];
export const INDENTS = [2, 4, 8];
export function buildFontSelect() {
  const sel = document.getElementById('fontSelect');
  for (const s of FONT_SIZES) { sel.appendChild(new Option(s + 'px', s)); }
  sel.value = 14;
  applyFontSize(14);
}
export function applyFontSize(n) { document.documentElement.style.setProperty('--editor-font', n + 'px'); for (const cm of S.cmPool) cm.refresh(); }
export function onFontChange() { const n = parseInt(document.getElementById('fontSelect').value) || 14; applyFontSize(n); persistSettings(); }
export function buildIndentSelect() {
  const sel = document.getElementById('indentSelect');
  for (const n of INDENTS) { sel.appendChild(new Option(n + ' sp', n)); }
  sel.value = 2;
  applyIndent(2);
}
export function applyIndent(n) { for (const cm of S.cmPool) { cm.setOption('indentUnit', n); cm.setOption('tabSize', n); } }
export function onIndentChange() { const n = parseInt(document.getElementById('indentSelect').value) || 2; applyIndent(n); document.getElementById('sbIndent').textContent = 'Spaces: ' + n; persistSettings(); }

// ── Settings: persisted by the backend in ~/.cppbox/cppbox.yaml ─────────
function persistSettings() {
  api('PUT', '/api/settings', {
    theme: document.getElementById('themeSelect').value,
    font_size: parseInt(document.getElementById('fontSelect').value) || 14,
    indent: parseInt(document.getElementById('indentSelect').value) || 2,
    std: document.getElementById('stdSelect').value,
  }).catch(() => { });
}
export function onThemeChange() { const id = document.getElementById('themeSelect').value; applyTheme(id); persistSettings(); }

// ── Run ──────────────────────────────────────────────────────────────────
let outHtml = '', compileHtml = '', stdinTimer = null;
function currentStdin() { const el = document.getElementById('stdinArea'); return el ? el.value : ''; }
function flushStdin() {
  const el = document.getElementById('stdinArea');
  if (!el || !S.project) return;
  const v = el.value; if (v === (S.project.stdin || '')) return;
  S.project.stdin = v;
  clearTimeout(stdinTimer);
  return api('PUT', `/api/projects/${S.project.id}`, { stdin: v }).catch(() => { });
}
export async function runCode(rebuild = false) {
  if (!S.project) { setStatus('Open a project first'); return; }
  await saveAllDirty();
  await flushStdin();
  const stdin = currentStdin();
  const b = document.getElementById(rebuild ? 'rebuildBtn' : 'runBtn'), meta = document.getElementById('outMeta');
  const label = (rebuild ? icon('refresh') + ' Rebuild' : icon('play') + ' Run');
  b.disabled = true; b.innerHTML = '<span class="spinner"></span> ' + (rebuild ? 'Rebuilding…' : 'Running…');
  switchOutTab('output');
  document.getElementById('outputContent').innerHTML = '<span style="color:var(--dim)">' + (rebuild ? 'make clean &amp;&amp; make…' : 'Compiling…') + '</span>';
  meta.textContent = '';
  const t0 = performance.now();
  try {
    const res = await api('POST', `/api/projects/${S.project.id}/${rebuild ? 'rebuild' : 'run'}`, { stdin });
    const secs = ((performance.now() - t0) / 1000).toFixed(2);
    const backendTag = res.backend ? ' · ' + res.backend : '';
    if (res.stage === 'compile' && !res.ok) {
      compileHtml = `<span class="err">Compilation failed:</span>\n${esc(res.compile_output)}`; outHtml = '';
      meta.textContent = 'compile error' + backendTag;
      renderOutTab(); switchOutTab('compile'); return;
    }
    compileHtml = esc(res.compile_output || '(no output)');
    outHtml = res.run_output ? esc(res.run_output) : '<span class="ok">Program ran successfully (no output).</span>';
    if (res.exit_code !== 0 && !res.timed_out) outHtml += `\n\n<span class="err">Exit code: ${res.exit_code}</span>`;
    if (res.timed_out) outHtml += '\n\n<span class="err">⚠ Timed out (30s limit)</span>';
    meta.textContent = (res.timed_out ? 'timed out' : 'exit ' + (res.exit_code != null ? res.exit_code : '?')) + ' · ' + secs + 's' + backendTag;
    renderOutTab();
  } catch (e) { outHtml = `<span class="err">Error:</span> ${esc(e.message)}`; renderOutTab(); }
  finally { b.disabled = false; b.innerHTML = label; }
}

// ── Format ───────────────────────────────────────────────────────────────
function isCppFile(p) { return /\.(cpp|cc|cxx|h|hpp|hh|hxx)$/i.test(p || ''); }
export async function formatCode(cm) {
  cm = cm || focusedCm();
  const path = S.cmPathMap.get(cm) || activeFilePath();
  if (!isCppFile(path)) { setStatus('Format applies to .cpp/.h/.hpp files only'); return; }
  const found = findTabByPath(path); if (!found || found.tab.missing) return;
  const tab = found.tab;
  const code = tab.doc.getValue(); if (!code.trim()) return;
  const b = document.getElementById('formatBtn'); b.disabled = true; b.innerHTML = '<span class="spinner"></span>';
  try {
    const indent = parseInt(document.getElementById('indentSelect').value) || 2;
    const res = await api('POST', '/api/format', { code, indent });
    if (res.formatted && res.formatted !== code) { tab.doc.setValue(res.formatted); tab.dirty = true; renderAllTabs(); scheduleAutoSave(path); setStatus('Formatted ✓'); }
    else setStatus('Already clean');
  } catch (e) { setStatus('Format error'); }
  finally { b.disabled = false; b.innerHTML = icon('wand') + ' Format'; if (!S.project) b.disabled = true; }
}
export function saveAndFormat(cm) { cm = cm || focusedCm(); saveActiveFile(cm); const path = S.cmPathMap.get(cm) || activeFilePath(); if (isCppFile(path)) setTimeout(() => formatCode(cm), 250); }

// ── Tests tab: per-project test cases, run against ./app before submit ──
let testsTimer = null, testsRunning = false;
function getTests() { return (S.project && S.project.tests) || []; }
function saveTests() {
  if (!S.project) return;
  clearTimeout(testsTimer);
  testsTimer = setTimeout(() => {
    api('PUT', `/api/projects/${S.project.id}`, { tests: JSON.stringify(getTests()) }).catch(e => flashErr(e));
  }, 500);
}
function renderTests() {
  const o = document.getElementById('outputContent');
  if (!S.project) { o.innerHTML = '<div style="color:var(--dim)">Open a project to define tests.</div>'; return; }
  const ts = getTests();
  const cases = ts.map((t, i) => `
    <div class="test-case" style="border:1px solid var(--border);border-radius:var(--r-sm);padding:8px;margin-bottom:8px;">
      <div style="display:flex;align-items:center;gap:8px;margin-bottom:6px;">
        <input class="t-name" data-i="${i}" value="${esc(t.name)}" placeholder="Case name" style="background:var(--surface2);border:1px solid var(--border);color:var(--text);padding:3px 8px;border-radius:var(--r-sm);font-size:12px;width:160px;outline:none;">
        <span class="t-badge" data-i="${i}" style="font-size:11px;color:var(--dim);">—</span>
        <button class="mini-btn del" data-i="${i}" onclick="delTest(${i})" title="Delete case">✕</button>
      </div>
      <div style="display:flex;gap:8px;">
        <div style="flex:1;"><label style="font-size:10px;color:var(--dim);">stdin</label><textarea class="t-stdin" data-i="${i}" rows="2" placeholder="program input">${esc(t.stdin)}</textarea></div>
        <div style="flex:1;"><label style="font-size:10px;color:var(--dim);">expected output</label><textarea class="t-expect" data-i="${i}" rows="2" placeholder="expected stdout">${esc(t.expect)}</textarea></div>
      </div>
      <div class="t-actual" data-i="${i}" style="display:none;font-size:11px;margin-top:4px;"></div>
    </div>`).join('');
  o.innerHTML = `
    <div style="display:flex;gap:8px;align-items:center;margin-bottom:8px;">
      <button class="btn btn-green" id="runTestsBtn" onclick="runTests()">${icon('play')} Run Tests</button>
      <button class="btn btn-ghost" onclick="addTest()">${icon('plus')} Add Case</button>
      <span id="testsSummary" style="font-size:11px;color:var(--dim);"></span>
    </div>
    ${cases || '<div style="color:var(--dim)">No test cases. Add one: type the program input and the output you expect, then Run Tests.</div>'}`;
  o.querySelectorAll('.t-name').forEach(el => el.onchange = () => { getTests()[el.dataset.i].name = el.value; saveTests(); });
  o.querySelectorAll('.t-stdin').forEach(el => el.onchange = () => { getTests()[el.dataset.i].stdin = el.value; saveTests(); });
  o.querySelectorAll('.t-expect').forEach(el => el.onchange = () => { getTests()[el.dataset.i].expect = el.value; saveTests(); });
}
export function addTest() { if (!S.project) return; const ts = getTests(); ts.push({ name: 'Case ' + (ts.length + 1), stdin: '', expect: '' }); S.project.tests = ts; renderTests(); saveTests(); }
export function delTest(i) { const ts = getTests(); ts.splice(i, 1); S.project.tests = ts; renderTests(); saveTests(); }
export async function runTests() {
  if (!S.project || testsRunning) return;
  await saveActiveFile(); await flushStdin();
  const ts = getTests(); if (!ts.length) { setStatus('No test cases'); return; }
  testsRunning = true;
  const btn = document.getElementById('runTestsBtn'); btn.disabled = true; btn.innerHTML = '<span class="spinner"></span> Running…';
  let pass = 0;
  for (let i = 0; i < ts.length; i++) {
    const badge = document.querySelector(`.t-badge[data-i="${i}"]`);
    const actualEl = document.querySelector(`.t-actual[data-i="${i}"]`);
    badge.textContent = '…'; badge.style.color = 'var(--dim)';
    try {
      const res = await api('POST', `/api/projects/${S.project.id}/run`, { stdin: ts[i].stdin });
      if (res.stage === 'compile' && !res.ok) { badge.textContent = '✕ compile error'; badge.style.color = 'var(--red)'; actualEl.style.display = 'block'; actualEl.innerHTML = '<span class="err">' + esc(res.compile_output) + '</span>'; break; }
      const ok = res.run_output.trim() === ts[i].expect.trim();
      if (ok) { pass++; badge.textContent = '✓ pass'; badge.style.color = 'var(--green)'; actualEl.style.display = 'none'; }
      else {
        badge.textContent = '✕ fail'; badge.style.color = 'var(--red)';
        actualEl.style.display = 'block';
        actualEl.innerHTML = '<span class="err">got:</span> ' + esc(res.run_output.trim() || '(no output)') + ' <span class="err">· expected:</span> ' + esc(ts[i].expect.trim());
      }
    } catch (e) { badge.textContent = '✕ ' + e.message; badge.style.color = 'var(--red)'; break; }
  }
  testsRunning = false; btn.disabled = false; btn.innerHTML = icon('play') + ' Run Tests';
  document.getElementById('testsSummary').textContent = `${pass}/${ts.length} passed`;
}

// ── Output pane tabs (output/compile/input/tests/problems) ─────────────
export function switchOutTab(t) {
  S.outTab = t; renderOutTab();
  document.querySelectorAll('.out-tab').forEach(x => { const on = x.dataset.tab === t; x.classList.toggle('active', on); x.setAttribute('aria-selected', on); });
}
export function renderOutTab() {
  const o = document.getElementById('outputContent');
  if (S.outTab === 'input') {
    o.innerHTML = '<textarea id="stdinArea" placeholder="Standard input for the program — sent on the next Run" aria-label="Program standard input"></textarea>';
    const ta = document.getElementById('stdinArea');
    ta.value = (S.project && S.project.stdin) || '';
    ta.oninput = () => { if (!S.project) return; clearTimeout(stdinTimer); stdinTimer = setTimeout(flushStdin, 600); };
    return;
  }
  if (S.outTab === 'tests') { renderTests(); return; }
  if (S.outTab === 'problems') {
    const items = problemsList();
    o.innerHTML = items.length ? items.map((d, i) =>
      `<div class="prob-item" onclick="jumpToProblem(${i})"><span class="sev" style="color:${d.sev === 'error' ? 'var(--red)' : d.sev === 'warning' ? 'var(--yellow)' : 'var(--dim)'}">${d.sev === 'error' ? '✕' : d.sev === 'warning' ? '⚠' : 'ℹ'}</span><span style="color:var(--dim)">${esc(d.file)}:${d.line}:${d.col}</span><span style="flex:1">${esc(d.msg)}</span></div>`).join('')
      : '<span style="color:var(--dim)">No problems. Diagnostics appear live as you type (clangd).</span>';
    return;
  }
  o.innerHTML = S.outTab === 'compile' ? (compileHtml || '<span style="color:var(--dim)">Run once to see the compile command and warnings.</span>') : (outHtml || '<span style="color:var(--dim)">Click Run (Ctrl+Enter) to compile &amp; execute.</span>');
}
function problemsList() {
  const out = [];
  for (const uri in S.lspDiags) for (const d of S.lspDiags[uri] || []) {
    if (!S.lsp.workspace || !S.project) continue;
    const prefix = 'file://' + S.lsp.workspace + '/' + S.project.id + '/';
    if (!uri.startsWith(prefix)) continue;
    const r = d.range || {}, s = r.start || { line: 0, character: 0 };
    out.push({ file: uri.slice(prefix.length), line: s.line + 1, col: s.character + 1, sev: d.severity === 1 ? 'error' : d.severity === 2 ? 'warning' : 'info', msg: d.message });
  }
  return out.sort((a, b) => a.file.localeCompare(b.file) || a.line - b.line);
}
export async function jumpToProblem(i) {
  const d = problemsList()[i]; if (!d) return;
  try { await revealFile(d.file, { line: d.line - 1, ch: d.col - 1 }); } catch (_) { }
}

// ── VS Code modal ────────────────────────────────────────────────────────
let vscodeInfo = null;
export async function showVscode() {
  try {
    if (!vscodeInfo) vscodeInfo = await api('GET', '/api/vscode');
    document.getElementById('vcUser').textContent = vscodeInfo.ssh_user;
    const projPath = S.project ? vscodeInfo.projects_root + '/' + S.project.id : vscodeInfo.projects_root;
    document.getElementById('vcPath').textContent = projPath;
    window._vcProjPath = projPath;
    const saved = localStorage.getItem('cppbox_ssh_host');
    document.getElementById('vcHostInput').value = saved || vscodeInfo.ssh_host;
    const ips = vscodeInfo.ips || [];
    document.getElementById('vcIps').innerHTML = ips.length ? ('suggest: ' + ips.map(ip => `<a href="#" onclick="event.preventDefault();document.getElementById('vcHostInput').value='${ip}';regenVscode()" style="color:var(--accent);margin-left:6px">${ip}</a>`).join('')) : '';
    regenVscode();
    document.getElementById('vscodeModal').classList.add('show');
  } catch (e) { flashErr(e); }
}
export function regenVscode() {
  if (!vscodeInfo) return;
  const host = (document.getElementById('vcHostInput').value || '').trim();
  const url = `vscode://vscode-remote/ssh-remote+${vscodeInfo.ssh_user}@${host}${window._vcProjPath}`;
  document.getElementById('vcUrl').textContent = url;
  document.getElementById('vcOpen').href = url;
  if (host) localStorage.setItem('cppbox_ssh_host', host);
}
export function hideVscode() { document.getElementById('vscodeModal').classList.remove('show'); }

// ── Submit modal ─────────────────────────────────────────────────────────
function fileListFromTree() {
  const out = [];
  (function walk(n) { if (!n) return; if (n.type === 'file') out.push(n.path); (n.children || []).forEach(walk); })(S.tree);
  return out;
}
export function showSubmit() {
  if (!S.project) { setStatus('Open a project to submit'); return; }
  document.getElementById('subProject').textContent = S.project.title;
  document.getElementById('subResult').textContent = '';
  const files = fileListFromTree().filter(p => !p.endsWith('Makefile') && !p.endsWith('.clangd'));
  document.getElementById('subFiles').innerHTML = files.length
    ? `<span style="color:var(--dim)">This exact snapshot (${files.length} files) is what gets graded:</span><br>` + files.map(f => `<span class="mfile">${esc(f)}</span>`).join('')
    : '<span style="color:var(--red)">No files in this project.</span>';
  document.getElementById('subKeyInput').value = localStorage.getItem('cppbox_subkey') || '';
  document.getElementById('submitModal').classList.add('show');
  api('GET', `/api/projects/${S.project.id}/submissions`).then(list => {
    document.getElementById('subHistory').innerHTML = list.length
      ? '<div style="font-size:11px;color:var(--dim);margin-top:6px;">Previous submissions:</div>' + list.map(s => `<div style="font-size:11px;color:var(--dim);">· #${s.counter} · ${timeAgo(s.at)}</div>`).join('')
      : '';
  }).catch(() => { });
}
export function hideSubmit() { document.getElementById('submitModal').classList.remove('show'); }
export async function doSubmit() {
  if (!S.project) return;
  const key = document.getElementById('subKeyInput').value.trim();
  const resEl = document.getElementById('subResult');
  if (!key) { resEl.innerHTML = '<span class="err">Enter a submission key</span>'; return; }
  const b = document.getElementById('subBtn');
  b.disabled = true; b.textContent = 'Submitting…'; resEl.textContent = '';
  try {
    await saveActiveFile();
    const r = await api('POST', '/api/submit', { key, project_id: S.project.id });
    localStorage.setItem('cppbox_subkey', key);
    resEl.innerHTML = '<span class="ok">Submitted ✓</span> ' + esc(r.zip) + ' <span style="color:var(--dim)">(submission #' + r.counter + ')</span>';
  } catch (e) {
    resEl.innerHTML = '<span class="err">' + esc(e.message) + '</span>';
  } finally {
    b.disabled = false; b.textContent = 'Submit';
  }
}

// exposed for inline HTML handlers
Object.assign(window, {
  createProject, openProject, renameProject, deleteProject,
  toggleTrash, restoreProject, purgeProject,
  toggleFolder, toggleTree, newFile, newFolder, renameNode, deleteNode,
  saveActiveFile, onStdChange, toggleGear, toggleTcPop, saveFlags, regenerateMakefile,
  onThemeChange, onFontChange, onIndentChange,
  runCode, formatCode, runTests, addTest, delTest,
  toggleDocs, docsHome, docsGo,
  switchOutTab, jumpToProblem,
  showVscode, regenVscode, hideVscode,
  showSubmit, hideSubmit, doSubmit,
});
