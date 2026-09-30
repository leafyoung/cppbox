// ── Admin panel: classes/students/keys, assignment actions (organize /
// pull / open workspace / grid), and the submission marking editor
// (file viewer + feedback text with @file autocomplete).
import { S } from './state.js';
import { api, esc, flashErr, setStatus } from './api.js';
import { loadProjects } from './projects.js';

let adminClasses = [], adminSelected = null, adminDetail = null;
export function showAdmin() { document.getElementById('adminPanel').classList.add('show'); loadAdminClasses(); loadWorkerSettings(); }
async function loadWorkerSettings() {
  try {
    const s = await api('GET', '/api/admin/settings/worker');
    document.getElementById('workerUrl').value = s.worker_url || '';
    document.getElementById('workerStatus').textContent = s.configured ? '✓ configured' : 'not set';
    document.getElementById('workerSecret').value = '';   // never echo the secret back into the field
  } catch (e) { }
}
export async function saveWorkerSettings() {
  const url = document.getElementById('workerUrl').value.trim();
  const secret = document.getElementById('workerSecret').value;
  try {
    const body = { worker_url: url }; if (secret) body.worker_secret = secret;
    const s = await api('PUT', '/api/admin/settings/worker', body);
    document.getElementById('workerStatus').textContent = s.configured ? '✓ saved & configured' : 'saved (need URL + secret)';
    document.getElementById('workerSecret').value = '';
    setStatus('Worker settings saved');
  } catch (e) { flashErr(e); }
}
export function genWorkerSecret() {
  const a = new Uint8Array(32); crypto.getRandomValues(a);   // 256-bit from the browser CSPRNG
  document.getElementById('workerSecret').value = [...a].map(b => b.toString(16).padStart(2, '0')).join('');
  document.getElementById('workerStatus').textContent = 'generated — also run: wrangler secret put ADMIN_SECRET (paste this)';
}
export function hideAdmin() { document.getElementById('adminPanel').classList.remove('show'); }

async function loadAdminClasses() {
  try { adminClasses = await api('GET', '/api/admin/classes'); renderAdminClassList(); } catch (e) { flashErr(e); }
}
function renderAdminClassList() {
  const el = document.getElementById('adminClassList');
  if (!adminClasses.length) { el.innerHTML = '<div class="hint">No classes yet.</div>'; return; }
  el.innerHTML = adminClasses.map(c => `
    <div class="admin-class-item ${adminSelected === c.id ? 'active' : ''}" onclick="selectClass('${c.id}')">
      <div class="nm">${esc(c.name)}</div>
      <div class="sub">${esc(c.course)} · ${esc(c.cohort)} · ${c.students} students · ${c.assignments} asg</div>
    </div>`).join('');
}
export async function selectClass(cid) {
  adminSelected = cid;
  try {
    adminDetail = await api('GET', `/api/admin/classes/${cid}`);
    renderAdminClassList();
    renderAdminRight();
  } catch (e) { flashErr(e); }
}
function renderAdminRight() {
  const el = document.getElementById('adminRight');
  const d = adminDetail;
  if (!d) { el.innerHTML = '<div class="admin-empty">Select a class to manage students, assignments, and keys.</div>'; return; }
  el.innerHTML = `
    <div class="admin-class-head">
      <h3>${esc(d.name)}</h3>
      <span class="meta">${esc(d.course)} · ${esc(d.cohort)} · ${d.students} students · ${d.assignments} assignments</span>
      <button class="mini" style="margin-left:auto;color:var(--red)" onclick="deleteClass('${d.id}')">Delete class</button>
    </div>
    <div class="admin-section">
      <div class="admin-label">Import students</div>
      <textarea class="admin-textarea" id="importText" placeholder="serial,name,email — one per line:&#10;1,Alice,alice@school.edu&#10;2,Bob,bob@school.edu&#10;10,Carol &lt;carol@school.edu&gt;"></textarea>
      <button class="btn btn-primary" onclick="importStudents()">Import</button>
      <span id="importResult" class="hint" style="margin-left:10px;"></span>
    </div>
    <div class="admin-section">
      <div class="admin-label">Roster (${d.student_list.length})</div>
      ${d.student_list.map(s => `<div class="roster-row"><span class="asg">${s.serial != null ? String(s.serial).padStart(2, '0') : '—'}</span><span class="nm">${esc(s.name)}</span><span class="em">${esc(s.email || '—')}</span></div>`).join('') || '<div class="hint">No students yet.</div>'}
    </div>
    <div class="admin-section">
      <div class="admin-label">Create assignment (generates a key for every student)</div>
      <div style="display:flex;gap:8px;align-items:center;flex-wrap:wrap;">
        <input class="admin-input" id="asgName" placeholder="Assignment name (e.g. Assignment 1)" style="flex:1;margin-bottom:0;">
        <input class="admin-input" id="asgSlot" type="number" value="${(d.assignment_list.length || 0) + 1}" style="width:70px;margin-bottom:0;" title="Slot number">
        <button class="btn btn-green" onclick="createAssignment()">Create &amp; Generate Keys</button>
      </div>
    </div>
    <div class="admin-section">
      <div class="admin-label">Assignments</div>
      ${d.assignment_list.length ? d.assignment_list.map(a => `
        <div class="asg-card">
          <div class="top"><span class="nm">${esc(a.name)}</span><span class="asg">slot ${a.slot}</span>${a.expires_ms ? `<span class="asg" title="Submission deadline">⏰ ${new Date(a.expires_ms).toLocaleString()}</span>` : ''}${(a.late_policy || 'filter') === 'reject' ? '<span class="asg" title="Late submissions are rejected at submission time">🚫 reject late</span>' : '<span class="asg" title="Late submissions are kept in the queue but skipped at pull">⬇ filter late</span>'}</div>
          <div class="row">
            <input class="admin-input" id="root_${a.id}" placeholder="Root folder path (e.g. /home/you/Asgn1)" value="${esc(a.root_folder || '')}" style="flex:1">
            <input class="admin-input" id="exp_${a.id}" type="datetime-local" value="${msToDTLocal(a.expires_ms)}" title="Submission deadline (empty = no expiry)" style="width:220px">
            <select class="admin-input" id="lp_${a.id}" title="Late submission policy" style="width:230px">
              <option value="filter" ${(a.late_policy || 'filter') !== 'reject' ? 'selected' : ''}>Late: receive &amp; filter at pull</option>
              <option value="reject" ${(a.late_policy || 'filter') === 'reject' ? 'selected' : ''}>Late: reject at submission</option>
            </select>
            <button class="mini" onclick="saveAsgSettings('${a.id}')">Save</button>
          </div>
          <div class="row">
            <input class="admin-input" id="zips_${a.id}" placeholder="Collected zips folder" style="flex:1">
            <button class="btn btn-primary" onclick="organizeAsg('${a.id}')">Organize</button>
            <button class="btn" onclick="pullAsg('${a.id}')">⬇ Pull</button>
            <button class="btn btn-green" onclick="openWorkspaceForAsg('${a.id}')">Open workspace</button>
            <button class="btn" onclick="viewGrid('${a.id}')">Grid</button>
          </div>
          <span class="hint" id="asgResult_${a.id}"></span>
        </div>`).join('') : '<div class="hint">No assignments yet.</div>'}
    </div>
    <div class="admin-section">
      <div class="admin-label">Keys</div>
      <div id="keysList"><div class="hint">Loading…</div></div>
    </div>`;
  loadClassKeys();
}
async function loadClassKeys() {
  if (!adminSelected) return;
  try {
    const keys = await api('GET', `/api/admin/classes/${adminSelected}/keys`);
    const el = document.getElementById('keysList');
    if (!keys.length) { el.innerHTML = '<div class="hint">No keys yet. Create an assignment to generate them.</div>'; return; }
    el.innerHTML = keys.map(k => `
      <div class="key-row">
        <div class="top">
          <span class="who">${esc(k.student_name)}</span>
          <span class="asg">${esc(k.assignment || ('slot ' + k.slot))}</span>
          ${k.email ? `<span class="hint">${esc(k.email)}</span>` : ''}
        </div>
        <div class="keyline">
          <code>${esc(k.key)}</code>
          <span class="acts">
            <button class="mini" onclick="copyText('${k.key}')">Copy</button>
            ${k.email ? `<a class="mini" href="${mailtoFor(k)}">✉ Email key</a>` : ''}
          </span>
        </div>
      </div>`).join('');
  } catch (e) { flashErr(e); }
}
function mailtoFor(k) {
  const subject = `[${k.course} ${k.cohort}] ${k.assignment || ('Slot ' + k.slot)} - Submission Key`;
  const body = `Hi ${k.student_name},\n\nYour submission key for ${k.assignment || ('assignment slot ' + k.slot)} (${k.course}, ${k.cohort}) is:\n\n    ${k.key}\n\nOpen your project in CPPBox, click Submit, and paste this key.\n\nRegards,\nInstructor`;
  return `mailto:${encodeURIComponent(k.email || '')}?subject=${encodeURIComponent(subject)}&body=${encodeURIComponent(body)}`;
}
export async function createClass() {
  const name = document.getElementById('className').value.trim();
  const course = document.getElementById('classCourse').value.trim();
  const cohort = document.getElementById('classCohort').value.trim();
  if (!name || !course || !cohort) { flashErr(new Error('Fill in name, course, and cohort')); return; }
  try {
    await api('POST', '/api/admin/classes', { name, course, cohort });
    document.getElementById('className').value = '';
    document.getElementById('classCourse').value = '';
    document.getElementById('classCohort').value = '';
    await loadAdminClasses();
  } catch (e) { flashErr(e); }
}
export async function deleteClass(cid) {
  if (!confirm('Delete this class, its students, and assignments?')) return;
  try {
    await api('DELETE', `/api/admin/classes/${cid}`);
    if (adminSelected === cid) { adminSelected = null; adminDetail = null; renderAdminRight(); }
    await loadAdminClasses();
  } catch (e) { flashErr(e); }
}
export async function importStudents() {
  const text = document.getElementById('importText').value;
  const resEl = document.getElementById('importResult');
  if (!text.trim()) { resEl.textContent = 'Paste a roster first'; return; }
  try {
    const r = await api('POST', `/api/admin/classes/${adminSelected}/students`, { text });
    resEl.textContent = `Added ${r.added} student(s)` + (r.errors && r.errors.length ? ` · ${r.errors.length} line(s) skipped (need a numeric serial first)` : '');
    document.getElementById('importText').value = '';
    await selectClass(adminSelected);
  } catch (e) { flashErr(e); }
}
export async function createAssignment() {
  const name = document.getElementById('asgName').value.trim();
  const slot = parseInt(document.getElementById('asgSlot').value, 10);
  if (!name || !slot) { flashErr(new Error('Enter assignment name and slot')); return; }
  try {
    const r = await api('POST', `/api/admin/classes/${adminSelected}/assignments`, { name, slot });
    await selectClass(adminSelected);
    setStatus(`Generated ${r.keys_generated} key(s)`);
  } catch (e) { flashErr(e); }
}

// ── Assignment actions: organize / workspace / grid / marking ───────────
let markerState = { pid: null, files: [], active: null, codeMirror: null, acIndex: -1 };
let gridState = { aid: null, data: null };

function _asgById(aid) { return (adminDetail && adminDetail.assignment_list || []).find(a => a.id === aid); }
export async function saveAsgSettings(aid) {
  const root = document.getElementById('root_' + aid).value.trim();
  const dv = document.getElementById('exp_' + aid).value;
  const ms = dv ? new Date(dv).getTime() : null;
  const lp = document.getElementById('lp_' + aid).value;
  try {
    const r = await api('PUT', `/api/admin/assignments/${aid}`, { root_folder: root, expires_ms: ms, late_policy: lp });
    if (adminDetail) { const a = _asgById(aid); if (a) { a.root_folder = r.root_folder; a.expires_ms = r.expires_ms; a.late_policy = r.late_policy; renderAdminRight(); } }
    setStatus('Assignment settings saved (Worker deadlines updated)');
  } catch (e) { flashErr(e); }
}
function msToDTLocal(ms) {
  if (!ms) return '';
  const d = new Date(ms), p = n => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}`;
}
export async function organizeAsg(aid) {
  const z = document.getElementById('zips_' + aid).value.trim();
  const res = document.getElementById('asgResult_' + aid);
  if (!z) { res.textContent = 'Enter the collected-zips folder first'; return; }
  res.textContent = 'Organizing…';
  try {
    const r = await api('POST', `/api/admin/assignments/${aid}/organize`, { zips_folder: z });
    res.textContent = `Organized ${r.organized.length} · ${r.errors.length} error(s)`;
  } catch (e) { res.textContent = ''; flashErr(e); }
}
export async function pullAsg(aid) {
  const res = document.getElementById('asgResult_' + aid);
  res.textContent = 'Pulling…';
  try {
    const r = await api('POST', `/api/admin/assignments/${aid}/pull`, {});
    if (r.error) { res.textContent = '⚠ ' + r.error; return; }
    res.textContent = `Pulled ${r.pulled} submission(s)` + (r.skipped_late ? ` · ${r.skipped_late} late (kept in queue)` : '') + (r.errors.length ? ` · ${r.errors.length} error(s)` : '');
    setStatus(`Pulled ${r.pulled} submission(s)` + (r.skipped_late ? `; ${r.skipped_late} late submission(s) left in the queue past the deadline` : '') + '; click Organize to unpack');
  } catch (e) { res.textContent = ''; flashErr(e); }
}
export async function openWorkspaceForAsg(aid) {
  const a = _asgById(aid);
  if (!a || !a.root_folder) { flashErr(new Error('Set a root folder first')); return; }
  try {
    const r = await api('POST', '/api/workspace/open', { root_folder: a.root_folder, assignment_id: aid });
    setStatus(`Opened ${r.opened} project(s); they appear in the sidebar`);
    await loadProjects();
  } catch (e) { flashErr(e); }
}
export async function viewGrid(aid) {
  gridState.aid = aid;
  document.getElementById('gridPanel').classList.add('show');
  await gridRefresh();
}
export function hideGrid() { document.getElementById('gridPanel').classList.remove('show'); }
export async function gridRefresh() {
  if (!gridState.aid) return;
  try {
    gridState.data = await api('GET', `/api/admin/assignments/${gridState.aid}/grid`);
    renderGrid();
  } catch (e) { flashErr(e); }
}
function renderGrid() {
  const d = gridState.data; if (!d) return;
  document.getElementById('gridTitle').textContent = d.assignment.name;
  const rows = d.students;
  const graded = rows.filter(s => s.status === 'graded').length;
  const sub = rows.filter(s => s.status !== 'none').length;
  document.getElementById('gridSummary').textContent = `${sub}/${rows.length} submitted · ${graded} graded`;
  const icon = { none: '—', submitted: '📩', graded: '✅' };
  document.getElementById('gridBody').innerHTML = rows.map(s => `
    <div class="grid-row">
      <span class="sn">${s.serial != null ? String(s.serial).padStart(2, '0') : '—'}</span>
      <span class="nm">${esc(s.name)} ${s.email ? '<span class=hint>' + esc(s.email) + '</span>' : ''}</span>
      <span class="grid-score">${s.score ? esc(s.score) : ''}</span>
      <span class="grid-status" title="${s.status}" onclick="gridClick('${s.student_id}',${s.project_id ? `'${s.project_id}'` : 'null'})">${icon[s.status]}</span>
    </div>`).join('');
}
export function gridClick(sid, pid) {
  if (!pid) { flashErr(new Error('No project for this student. Organize + Open workspace first.')); return; }
  const s = (gridState.data.students || []).find(x => x.student_id === sid);
  openMarker(pid, s);
}
export async function openMarker(pid, s) {
  markerState.pid = pid; markerState.active = null; markerState.files = []; markerState.student = s || null;
  document.getElementById('markerPanel').classList.add('show');
  document.getElementById('markerTitle').textContent = s ? `Marking · ${String(s.serial || '?').padStart(2, '0')} ${s.name}` : 'Marking';
  document.getElementById('markerMeta').textContent = '';
  document.getElementById('feedbackScore').value = (s && s.score) || '';
  document.getElementById('feedbackStatus').textContent = '';
  document.getElementById('feedbackTa').value = '';
  document.getElementById('markerFiles').innerHTML = '<span class=hint>Loading…</span>';
  // lazy init read-only CodeMirror
  if (!markerState.codeMirror) {
    markerState.codeMirror = CodeMirror(document.getElementById('markerCode'), { mode: 'text/x-c++src', theme: S.cmPool[0].getOption('theme'), readonly: true, lineNumbers: true });
  }
  markerState.codeMirror.setValue('// select a file');
  markerState.codeMirror.refresh();
  await loadMarkerFiles();
  await loadFeedback();
}
export function hideMarker() { document.getElementById('markerPanel').classList.remove('show'); }
export function emailFeedback() {
  const s = markerState.student;
  if (!s || !s.email) { flashErr(new Error('No email on file for this student')); return; }
  const text = document.getElementById('feedbackTa').value;
  const score = document.getElementById('feedbackScore').value.trim();
  const subject = `Feedback: ${(s.serial != null ? ('#' + s.serial + ' ') : '')}${s.name}`;
  const body = text + (score ? `\n\nScore: ${score}` : '');
  window.location.href = `mailto:${encodeURIComponent(s.email)}?subject=${encodeURIComponent(subject)}&body=${encodeURIComponent(body)}`;
}
function _flattenFiles(nodes, acc) {
  for (const n of nodes) { if (n.type === 'file') acc.push(n.path); else if (n.children) _flattenFiles(n.children, acc); }
  return acc;
}
async function loadMarkerFiles() {
  try {
    const tree = await api('GET', `/api/projects/${markerState.pid}/tree`);
    markerState.files = _flattenFiles(tree.children || [], []).filter(p => p !== 'feedback.md');
    const el = document.getElementById('markerFiles');
    el.innerHTML = markerState.files.length ? markerState.files.map(p => `<span class="mfile" onclick="showMarkerFile('${p.replace(/'/g, "\\'")}')">${esc(p)}</span>`).join('') : '<span class=hint>No files</span>';
  } catch (e) { flashErr(e); }
}
export async function showMarkerFile(path) {
  markerState.active = path;
  document.querySelectorAll('.mfile').forEach(el => el.classList.toggle('active', el.textContent === path));
  try {
    const r = await api('GET', `/api/projects/${markerState.pid}/file?path=${encodeURIComponent(path)}`);
    markerState.codeMirror.setValue(r.content); markerState.codeMirror.refresh();
  } catch (e) { flashErr(e); }
}
async function loadFeedback() {
  try {
    const f = await api('GET', `/api/admin/projects/${markerState.pid}/feedback`);
    document.getElementById('feedbackTa').value = f.text || '';
    document.getElementById('feedbackScore').value = f.score || '';
    document.getElementById('feedbackStatus').textContent = f.graded ? '✅ published' : 'draft';
  } catch (e) { flashErr(e); }
}
export async function saveFeedbackDraft() {
  try {
    await api('POST', `/api/admin/projects/${markerState.pid}/feedback`, { text: document.getElementById('feedbackTa').value, score: document.getElementById('feedbackScore').value.trim() || null, publish: false });
    document.getElementById('feedbackStatus').textContent = 'saved (draft)';
  } catch (e) { flashErr(e); }
}
export async function publishFeedback() {
  try {
    await api('POST', `/api/admin/projects/${markerState.pid}/feedback`, { text: document.getElementById('feedbackTa').value, score: document.getElementById('feedbackScore').value.trim() || null, publish: true });
    document.getElementById('feedbackStatus').textContent = '✅ published';
    setStatus('Feedback published');
    if (gridState.aid) gridRefresh();
  } catch (e) { flashErr(e); }
}
// @-autocomplete in feedback textarea
function _acPos() { const ta = document.getElementById('feedbackTa'); const v = ta.value.slice(0, ta.selectionStart); const i = v.lastIndexOf('@'); if (i < 0) return null; const word = v.slice(i + 1); if (/[\s,;()]/.test(word)) return null; return { at: i, word, ta }; }
function feedbackAcShow(matches) {
  const pos = _acPos(); const pop = document.getElementById('acPop');
  if (!pos || !matches.length) { pop.style.display = 'none'; return; }
  markerState.acIndex = 0;
  pop.innerHTML = matches.map((m, i) => `<div class="ac-item${i === 0 ? ' active' : ''}" onclick="feedbackAcPick('${m.replace(/'/g, "\\'")}')">${esc(m)}</div>`).join('');
  const r = pos.ta.getBoundingClientRect(); pop.style.display = 'block'; pop.style.left = (r.left + 6) + 'px'; pop.style.top = (r.bottom + 2) + 'px';
}
export function feedbackAcPick(m) {
  const ta = document.getElementById('feedbackTa'); const pos = _acPos(); if (!pos) return;
  ta.value = ta.value.slice(0, pos.at + 1) + m + ta.value.slice(pos.at + 1 + pos.word.length);
  const c = pos.at + 1 + m.length; ta.focus(); ta.setSelectionRange(c, c); document.getElementById('acPop').style.display = 'none';
}
document.addEventListener('input', e => { if (e.target && e.target.id === 'feedbackTa') { const pos = _acPos(); const pop = document.getElementById('acPop'); if (!pos) { pop.style.display = 'none'; return; } const ms = markerState.files.filter(f => f.toLowerCase().includes(pos.word.toLowerCase())).slice(0, 12); feedbackAcShow(ms); } });
document.addEventListener('keydown', e => {
  if (e.target && e.target.id === 'feedbackTa') {
    const pop = document.getElementById('acPop'); if (pop.style.display !== 'block') return; const items = pop.querySelectorAll('.ac-item'); if (e.key === 'ArrowDown') { e.preventDefault(); markerState.acIndex = Math.min(markerState.acIndex + 1, items.length - 1); updateAcSel(items); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); markerState.acIndex = Math.max(markerState.acIndex - 1, 0); updateAcSel(items); }
    else if (e.key === 'Enter' || e.key === 'Tab') { if (items[markerState.acIndex]) { e.preventDefault(); feedbackAcPick(items[markerState.acIndex].textContent); } }
    else if (e.key === 'Escape') { pop.style.display = 'none'; }
  }
});
function updateAcSel(items) { items.forEach((el, i) => el.classList.toggle('active', i === markerState.acIndex)); }

// exposed for inline HTML handlers
Object.assign(window, {
  saveWorkerSettings, genWorkerSecret, hideAdmin,
  selectClass, createClass, deleteClass, importStudents, createAssignment,
  saveAsgSettings, organizeAsg, pullAsg, openWorkspaceForAsg, viewGrid, hideGrid, gridRefresh, gridClick,
  hideMarker, emailFeedback, showMarkerFile, saveFeedbackDraft, publishFeedback, feedbackAcPick,
  showAdmin,
});
