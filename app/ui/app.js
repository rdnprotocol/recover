'use strict';

/* ---------- Tauri bridge (with a browser mock for UI development) ---------- */
const T = window.__TAURI__;
const invoke = T ? T.core.invoke : (cmd, args) => Mock.invoke(cmd, args);
const listen = T ? T.event.listen : (ev, cb) => Mock.listen(ev, cb);
async function pickFolder() {
  if (!T) return '/tmp/recovered';
  if (T.dialog && T.dialog.open) return T.dialog.open({ directory: true, title: 'Хадгалах хавтас сонгох' });
  return invoke('plugin:dialog|open', { options: { directory: true } });
}
async function pickImage() {
  if (!T) return '/tmp/disk.img';
  const opts = { multiple: false, title: 'Disk image сонгох', filters: [{ name: 'Disk image', extensions: ['img', 'dd', 'raw', 'bin', 'iso', 'vhd'] }, { name: 'Бүх файл', extensions: ['*'] }] };
  if (T.dialog && T.dialog.open) return T.dialog.open(opts);
  return invoke('plugin:dialog|open', { options: opts });
}

/* ---------- helpers ---------- */
const $ = (s) => document.querySelector(s);
const esc = (s) => String(s ?? '').replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
function human(n) {
  if (n == null) return '';
  const u = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0, v = n;
  while (v >= 1024 && i < 4) { v /= 1024; i++; }
  return i === 0 ? `${n} B` : `${v.toFixed(v < 10 ? 2 : 1)} ${u[i]}`;
}
function fmtDate(s, naive) {
  if (!s) return '—';
  const d = new Date(s * 1000);
  const p = (x) => String(x).padStart(2, '0');
  // FAT/exFAT times are local wall-clock values stored without a zone.
  if (naive) return `${d.getUTCFullYear()}.${p(d.getUTCMonth() + 1)}.${p(d.getUTCDate())} ${p(d.getUTCHours())}:${p(d.getUTCMinutes())}`;
  return `${d.getFullYear()}.${p(d.getMonth() + 1)}.${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}
function fmtTime(sec) {
  sec = Math.max(0, Math.round(sec));
  const h = Math.floor(sec / 3600), m = Math.floor((sec % 3600) / 60), s = sec % 60;
  return h ? `${h}:${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}` : `${m}:${String(s).padStart(2, '0')}`;
}
let toastTimer;
function toast(msg, err = false) {
  const t = $('#toast');
  t.textContent = msg;
  t.className = 'toast' + (err ? ' err' : '');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.classList.add('hidden'), err ? 6000 : 3000);
}

const CATS = [
  ['all', 'Бүгд', 'var(--brand)'],
  ['image', 'Зураг', 'var(--c-image)'],
  ['document', 'Баримт бичиг', 'var(--c-document)'],
  ['video', 'Видео', 'var(--c-video)'],
  ['audio', 'Аудио', 'var(--c-audio)'],
  ['archive', 'Архив', 'var(--c-archive)'],
  ['other', 'Бусад', 'var(--c-other)'],
];
const CAT_COLOR = Object.fromEntries(CATS.map((c) => [c[0], c[2]]));
const QUALITY = {
  good: ['Сайн', 'Өгөгдөл дарагдаагүй — бүрэн сэргээгдэх магадлал өндөр.'],
  unknown: ['Тодорхойгүй', 'Файлын бүтэц хэсэгчлэн тодорхойгүй. Сэргээгээд нээж шалгана уу.'],
  poor: ['Гэмтсэн байж магадгүй', 'Файлын зарим хэсгийг өөр өгөгдөл дарж бичсэн байж болзошгүй.'],
};
const SOURCE = { ntfs: 'NTFS бичлэг', fat: 'FAT бичлэг', exfat: 'exFAT бичлэг', carved: 'Гүн скан (signature)' };

/* ---------- state ---------- */
const S = {
  device: null,
  devices: [],
  filter: { category: 'all', deletedOnly: true, search: '', quality: '', sort: '', desc: false },
  rows: [],
  total: 0,
  totalBytes: 0,
  selected: new Map(), // id -> size
  selectAll: false,
  activeId: null,
  scanStart: 0,
  lastProgress: null,
};

function show(screen) {
  document.querySelectorAll('.screen').forEach((s) => s.classList.toggle('active', s.id === screen));
  const order = ['home', 'scan', 'results'];
  document.querySelectorAll('#steps span').forEach((s) => {
    const i = order.indexOf(s.dataset.step), cur = order.indexOf(screen);
    s.classList.toggle('on', i === cur);
    s.classList.toggle('done', i < cur);
  });
}

/* ---------- 1. devices ---------- */
const ICONS = {
  volume: '<svg viewBox="0 0 24 24"><rect x="3" y="7" width="18" height="10" rx="2"/><circle cx="17" cy="12" r="1.1"/><path d="M6.5 12h6"/></svg>',
  disk: '<svg viewBox="0 0 24 24"><rect x="4" y="3" width="16" height="18" rx="2.5"/><circle cx="12" cy="11" r="4"/><circle cx="12" cy="11" r=".8"/><path d="M15.5 17.5h1"/></svg>',
  image: '<svg viewBox="0 0 24 24"><path d="M6 3h8l4 4v14H6z"/><path d="M14 3v4h4"/><rect x="9" y="11" width="6" height="6" rx="1"/></svg>',
};

function deviceCard(d) {
  const b = document.createElement('button');
  b.className = 'card';
  b.dataset.path = d.path;
  const title = d.label ? `${esc(d.name)} <span class="muted" style="font-weight:400">${esc(d.label)}</span>` : esc(d.name);
  const fs = d.filesystem ? `<span class="fs">${esc(d.filesystem)}</span>` : (d.kind === 'disk' ? '' : '<span class="fs none">RAW</span>');
  b.innerHTML = `<div class="card-icon">${ICONS[d.kind] || ICONS.volume}</div>
    <div class="card-body"><div class="card-title">${title} ${fs}</div>
    <div class="card-sub">${human(d.size)} · ${esc(d.path)}</div></div>`;
  b.onclick = () => selectDevice(d, b);
  return b;
}

function selectDevice(d, el) {
  S.device = d;
  document.querySelectorAll('.card').forEach((c) => c.classList.remove('selected'));
  el.classList.add('selected');
  $('#scanBtn').disabled = false;
  // A RAW/unknown volume can only be scanned deeply.
  if (d.kind !== 'disk' && !d.filesystem) document.querySelector('input[name=mode][value=deep]').checked = true;
}

async function loadDevices() {
  const vols = $('#volumes');
  vols.innerHTML = '<div class="skeleton"></div><div class="skeleton"></div><div class="skeleton"></div>';
  document.querySelectorAll('#disks .card:not(.image-card)').forEach((c) => c.remove());
  const [elevated, list] = await Promise.all([invoke('is_elevated').catch(() => true), invoke('list_devices').catch(() => [])]);
  S.devices = list;
  $('#adminWarn').classList.toggle('hidden', elevated);
  $('#adminBadge').innerHTML = elevated ? '<span class="pill">Админ эрхтэй</span>' : '<span class="pill bad">Админ эрхгүй</span>';
  vols.innerHTML = '';
  const v = list.filter((d) => d.kind === 'volume');
  if (!v.length) vols.innerHTML = '<p class="muted span">Боть олдсонгүй. Администратор эрхээр ажиллуулж байгаа эсэхээ шалгана уу.</p>';
  v.forEach((d) => vols.appendChild(deviceCard(d)));
  const disks = $('#disks');
  list.filter((d) => d.kind === 'disk').forEach((d) => disks.insertBefore(deviceCard(d), $('#imageBtn')));
  S.device = null;
  $('#scanBtn').disabled = true;
}

$('#refreshBtn').onclick = loadDevices;
$('#imageBtn').onclick = async () => {
  const path = await pickImage();
  if (!path) return;
  try {
    const info = await invoke('open_image', { path });
    const card = deviceCard(info);
    $('#disks').insertBefore(card, $('#imageBtn'));
    selectDevice(info, card);
  } catch (e) { toast(String(e), true); }
};

/* ---------- 2. scan ---------- */
let scanTimer;
$('#scanBtn').onclick = async () => {
  if (!S.device) return;
  const deep = document.querySelector('input[name=mode]:checked').value === 'deep';
  try {
    await invoke('start_scan', { path: S.device.path, deep });
  } catch (e) { toast(String(e), true); return; }
  S.scanStart = Date.now();
  S.lastProgress = null;
  $('#scan').classList.remove('scan-done');
  $('#scanTitle').textContent = deep ? 'Гүн скан хийж байна…' : 'Хурдан скан хийж байна…';
  $('#scanDevice').textContent = `${S.device.name}${S.device.label ? ' · ' + S.device.label : ''} · ${human(S.device.size)}`;
  $('#scanPhase').textContent = 'Бэлдэж байна…';
  $('#scanBar').style.width = '0%';
  $('#scanPct').textContent = '0%';
  $('#scanFound').textContent = '0';
  $('#scanEta').textContent = '—';
  $('#cancelScanBtn').disabled = false;
  show('scan');
  clearInterval(scanTimer);
  scanTimer = setInterval(() => { $('#scanTime').textContent = fmtTime((Date.now() - S.scanStart) / 1000); }, 500);
};
$('#cancelScanBtn').onclick = () => { invoke('cancel'); $('#cancelScanBtn').disabled = true; $('#scanPhase').textContent = 'Зогсоож байна…'; };

const PHASE = { fs: '1-р шат · Файлын системийн бичлэг уншиж байна', carve: '2-р шат · Гүн скан — салбар бүрийг шалгаж байна' };
listen('scan-progress', (e) => {
  const p = e.payload;
  if (p.phase === 'done') return;
  const pct = p.total ? (p.done / p.total) * 100 : 0;
  $('#scanPhase').textContent = PHASE[p.phase] || p.message;
  $('#scanBar').style.width = pct.toFixed(1) + '%';
  $('#scanPct').textContent = pct.toFixed(pct < 10 ? 1 : 0) + '%';
  $('#scanFound').textContent = p.found.toLocaleString('en-US');
  const now = Date.now();
  if (p.phase === 'carve' && S.lastProgress && S.lastProgress.phase === 'carve' && p.done > S.lastProgress.startDone) {
    const rate = (p.done - S.lastProgress.startDone) / ((now - S.lastProgress.startAt) / 1000);
    if (rate > 0) $('#scanEta').textContent = fmtTime((p.total - p.done) / rate);
  } else if (!S.lastProgress || S.lastProgress.phase !== p.phase) {
    S.lastProgress = { phase: p.phase, startDone: p.done, startAt: now };
  }
});

listen('scan-done', async (e) => {
  clearInterval(scanTimer);
  const d = e.payload;
  $('#scan').classList.add('scan-done');
  const vols = d.volumes.map((v) => `${v.filesystem}${v.label ? ' (' + esc(v.label) + ')' : ''}`).join(', ') || 'файлын систем танигдсангүй';
  $('#scanSummary').innerHTML = `<div>${esc(S.device.name)}</div><div>${vols}</div>
    <div>${d.total.toLocaleString('en-US')} файл · ${d.deleted.toLocaleString('en-US')} устгагдсан</div>
    <div>Хугацаа: ${fmtTime(d.seconds)}${d.cancelled ? ' · <b>зогсоосон</b>' : ''}</div>
    ${d.errors.map((x) => `<div class="err">${esc(x)}</div>`).join('')}`;
  S.selected.clear();
  S.selectAll = false;
  S.activeId = null;
  resetPreview();
  // Volumes with no deleted entries: show everything instead of an empty list.
  $('#deletedOnly').checked = d.deleted > 0 || d.total === 0;
  S.filter = { category: 'all', deletedOnly: $('#deletedOnly').checked, search: '', quality: '', sort: '', desc: false };
  $('#search').value = '';
  $('#qualityFilter').value = '';
  show('results');
  await refreshAll();
});

/* ---------- 3. results ---------- */
async function refreshAll() {
  await loadSummary();
  await loadRows(true);
}

async function loadSummary() {
  const s = await invoke('summary', { deletedOnly: S.filter.deletedOnly });
  const ul = $('#categories');
  ul.innerHTML = '';
  CATS.forEach(([key, label, color]) => {
    const n = key === 'all' ? s.total : s.categories[key] || 0;
    if (key !== 'all' && !n) return;
    const li = document.createElement('li');
    li.className = S.filter.category === key ? 'on' : '';
    li.innerHTML = `<span class="dot" style="background:${color}"></span>${label}<span class="n">${n.toLocaleString('en-US')}</span>`;
    li.onclick = () => { S.filter.category = key; S.selectAll = false; loadSummary(); loadRows(true); };
    ul.appendChild(li);
  });
}

const PAGE = 400;
async function loadRows(reset) {
  const offset = reset ? 0 : S.rows.length;
  let page;
  try {
    page = await invoke('query', { filter: S.filter, offset, limit: PAGE });
  } catch (e) { toast(String(e), true); return; }
  S.total = page.total;
  S.totalBytes = page.total_bytes;
  if (reset) { S.rows = []; $('#rows').innerHTML = ''; $('#tableWrap').scrollTop = 0; }
  S.rows.push(...page.items);
  const frag = document.createDocumentFragment();
  page.items.forEach((f) => frag.appendChild(rowEl(f)));
  $('#rows').appendChild(frag);
  $('#empty').classList.toggle('hidden', S.total > 0);
  $('#moreBtn').classList.toggle('hidden', S.rows.length >= S.total);
  $('#moreBtn').textContent = `Цааш ачаалах (${(S.total - S.rows.length).toLocaleString('en-US')} үлдсэн)`;
  $('#resultCount').textContent = `${S.total.toLocaleString('en-US')} файл · ${human(S.totalBytes)}`;
  document.querySelectorAll('th[data-sort]').forEach((th) => {
    th.classList.toggle('sorted', th.dataset.sort === S.filter.sort);
    th.classList.toggle('asc', !S.filter.desc);
  });
  updateSelection();
}

// Show the tail of long folder paths ("…/Баримт/2026"), full path in tooltip.
function shortPath(p) {
  if (!p) return '/';
  if (p.length <= 24) return p;
  const parts = p.split('/');
  let out = parts.pop();
  while (parts.length && out.length + parts[parts.length - 1].length < 22) out = parts.pop() + '/' + out;
  return '…/' + out;
}

function rowEl(f) {
  const tr = document.createElement('tr');
  tr.dataset.id = f.id;
  if (f.id === S.activeId) tr.className = 'active';
  const ext = (f.ext || '?').slice(0, 4);
  const badge = f.deleted ? `<span class="badge ${f.quality}">${QUALITY[f.quality][0]}</span>` : '<span class="badge live">Байгаа</span>';
  tr.innerHTML = `<td class="c-check"><input type="checkbox" ${S.selectAll || S.selected.has(f.id) ? 'checked' : ''}></td>
    <td><div class="fname"><div class="ficon" style="background:${CAT_COLOR[f.category]}">${esc(ext)}</div><span title="${esc(f.name)}">${esc(f.name)}</span></div></td>
    <td class="path" title="${esc(f.path)}">${esc(shortPath(f.path))}</td>
    <td class="num">${human(f.size)}</td>
    <td class="date">${fmtDate(f.modified, f.naive_time)}</td>
    <td>${badge}</td>`;
  const cb = tr.querySelector('input');
  cb.onclick = (ev) => { ev.stopPropagation(); toggleSelect(f, cb.checked); };
  tr.onclick = () => activate(f, tr);
  tr.ondblclick = () => { cb.checked = !cb.checked; toggleSelect(f, cb.checked); };
  return tr;
}

function toggleSelect(f, on) {
  if (S.selectAll) {
    // leave "select everything" mode: keep the loaded rows, minus this one
    S.selectAll = false;
    S.rows.forEach((r) => S.selected.set(r.id, r.size));
  }
  if (on) S.selected.set(f.id, f.size); else S.selected.delete(f.id);
  updateSelection();
}

function updateSelection() {
  const n = S.selectAll ? S.total : S.selected.size;
  const bytes = S.selectAll ? S.totalBytes : [...S.selected.values()].reduce((a, b) => a + b, 0);
  $('#selInfo').innerHTML = n ? `<b>${n.toLocaleString('en-US')}</b> файл сонгосон · ${human(bytes)}` : 'Сэргээх файлаа сонгоно уу';
  $('#recoverBtn').disabled = n === 0;
  $('#checkAll').checked = S.selectAll;
  $('#checkAll').indeterminate = !S.selectAll && S.selected.size > 0;
}

$('#checkAll').onclick = () => {
  S.selectAll = $('#checkAll').checked;
  if (!S.selectAll) S.selected.clear();
  document.querySelectorAll('#rows input').forEach((c) => (c.checked = S.selectAll));
  updateSelection();
};
$('#moreBtn').onclick = () => loadRows(false);
$('#deletedOnly').onchange = (e) => { S.filter.deletedOnly = e.target.checked; S.selectAll = false; refreshAll(); };
$('#qualityFilter').onchange = (e) => { S.filter.quality = e.target.value; S.selectAll = false; loadRows(true); };
let searchT;
$('#search').oninput = (e) => { clearTimeout(searchT); searchT = setTimeout(() => { S.filter.search = e.target.value; S.selectAll = false; loadRows(true); }, 220); };
document.querySelectorAll('th[data-sort]').forEach((th) => {
  th.onclick = () => {
    const k = th.dataset.sort;
    if (S.filter.sort === k) S.filter.desc = !S.filter.desc;
    else { S.filter.sort = k; S.filter.desc = k === 'size' || k === 'date'; }
    loadRows(true);
  };
});
$('#backBtn').onclick = () => { show('home'); };

/* preview */
function resetPreview() {
  $('#preview').innerHTML = `<div class="preview-empty"><svg viewBox="0 0 48 48"><rect x="8" y="8" width="32" height="32" rx="4"/><circle cx="18" cy="19" r="3"/><path d="M10 36l10-10 6 6 5-5 9 9"/></svg><p>Файл дээр дарж урьдчилан харна уу</p></div>`;
}
let pvToken = 0;
async function activate(f, tr) {
  S.activeId = f.id;
  document.querySelectorAll('#rows tr.active').forEach((r) => r.classList.remove('active'));
  tr.classList.add('active');
  const token = ++pvToken;
  const q = f.deleted ? f.quality : 'good';
  $('#preview').innerHTML = `<div class="pv-media" id="pvMedia"><div class="spinner"></div></div>
    <div class="pv-name">${esc(f.name)}</div>
    <dl class="pv-meta">
      <dt>Хэмжээ</dt><dd>${human(f.size)} (${f.size.toLocaleString('en-US')} байт)</dd>
      <dt>Хавтас</dt><dd>${esc(f.path || '/')}</dd>
      <dt>Огноо</dt><dd>${fmtDate(f.modified, f.naive_time)}</dd>
      <dt>Олдсон</dt><dd>${SOURCE[f.source] || f.source}</dd>
      <dt>Төлөв</dt><dd>${f.deleted ? 'Устгагдсан' : 'Устгагдаагүй'}${f.fragments > 1 ? ` · ${f.fragments} хэсэгт хуваагдсан` : ''}</dd>
      ${f.offset != null ? `<dt>Байрлал</dt><dd class="mono">0x${f.offset.toString(16).toUpperCase()}</dd>` : ''}
    </dl>
    ${f.deleted ? `<div class="pv-note ${q}">${QUALITY[q][1]}</div>` : ''}`;
  try {
    const p = await invoke('preview', { id: f.id });
    if (token !== pvToken) return;
    const m = $('#pvMedia');
    if (p.kind === 'image') {
      const img = new Image();
      img.onerror = () => { m.innerHTML = '<p class="muted" style="padding:20px;text-align:center">Зургийг харуулах боломжгүй — файл гэмтсэн байж магадгүй.</p>'; };
      img.src = `data:${p.mime};base64,${p.data}`;
      m.innerHTML = '';
      m.appendChild(img);
    } else if (p.kind === 'text') {
      m.innerHTML = `<pre class="pv-text">${esc(p.data)}</pre>`;
    } else {
      m.innerHTML = `<pre>${esc(p.data)}</pre>`;
    }
  } catch (e) {
    if (token === pvToken) $('#pvMedia').innerHTML = `<p class="muted" style="padding:16px">${esc(e)}</p>`;
  }
}

/* ---------- recover dialog ---------- */
let lastOut = '';
$('#recoverBtn').onclick = () => {
  const n = S.selectAll ? S.total : S.selected.size;
  const bytes = S.selectAll ? S.totalBytes : [...S.selected.values()].reduce((a, b) => a + b, 0);
  $('#recWhat').textContent = `${n.toLocaleString('en-US')} файл (${human(bytes)}) сэргээнэ.`;
  $('#recSetup').classList.remove('hidden');
  $('#recRun').classList.add('hidden');
  $('#recDone').classList.add('hidden');
  $('#recTitle').textContent = 'Файл сэргээх';
  $('#outDir').value = lastOut;
  checkOut();
  $('#recoverModal').classList.remove('hidden');
};
async function checkOut() {
  const dir = $('#outDir').value;
  let same = false;
  if (dir) same = await invoke('check_destination', { outDir: dir }).catch(() => false);
  $('#sameVolWarn').classList.toggle('hidden', !same);
  $('#recStart').disabled = !dir || same;
}
$('#pickDir').onclick = async () => {
  const d = await pickFolder();
  if (d) { $('#outDir').value = d; lastOut = d; checkOut(); }
};
$('#recCancel').onclick = $('#recClose').onclick = () => $('#recoverModal').classList.add('hidden');
$('#recStart').onclick = async () => {
  const args = {
    ids: S.selectAll ? [] : [...S.selected.keys()],
    filter: S.selectAll ? S.filter : null,
    outDir: $('#outDir').value,
    keepPaths: $('#keepPaths').checked,
  };
  try { await invoke('start_recover', args); } catch (e) { toast(String(e), true); return; }
  $('#recSetup').classList.add('hidden');
  $('#recRun').classList.remove('hidden');
  $('#recTitle').textContent = 'Сэргээж байна…';
  $('#recBar').style.width = '0%';
};
$('#recStop').onclick = () => invoke('cancel');
listen('recover-progress', (e) => {
  const [d, t, name] = e.payload;
  $('#recBar').style.width = (t ? (d / t) * 100 : 0).toFixed(1) + '%';
  $('#recFile').textContent = name ? `${d + 1}/${t} · ${name}` : '';
});
listen('recover-done', (e) => {
  const r = e.payload;
  $('#recRun').classList.add('hidden');
  $('#recDone').classList.remove('hidden');
  $('#recTitle').textContent = r.failed && !r.ok ? 'Сэргээж чадсангүй' : 'Сэргээлт дууслаа';
  $('#recResult').innerHTML = `<b>${r.ok.toLocaleString('en-US')}</b> файл амжилттай (${human(r.bytes)})${r.failed ? `, <b>${r.failed}</b> алдаатай` : ''}.`;
  $('#recErrors').classList.toggle('hidden', !r.errors.length);
  $('#recErrors').textContent = r.errors.join('\n');
  $('#recOpen').classList.toggle('hidden', !r.ok);
  $('#recOpen').onclick = () => invoke('open_folder', { path: r.out_dir });
});

document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && !$('#recoverModal').classList.contains('hidden') && $('#recRun').classList.contains('hidden')) {
    $('#recoverModal').classList.add('hidden');
  }
});
document.addEventListener('contextmenu', (e) => { if (T && !e.target.closest('input, pre, .pv-meta')) e.preventDefault(); });

/* ---------- mock backend (browser preview only) ---------- */
const Mock = {
  handlers: {},
  listen(ev, cb) { (this.handlers[ev] ||= []).push(cb); return Promise.resolve(() => {}); },
  emit(ev, payload) { (this.handlers[ev] || []).forEach((cb) => cb({ payload })); },
  files: [],
  async invoke(cmd, a) {
    const names = [['Тайлан_2026_Q3.docx', 'document', 'Documents/Тайлан'], ['Гэрээ_эцсийн.pdf', 'document', 'Documents'], ['IMG_20260914_1532.jpg', 'image', 'DCIM/Camera'], ['Төсөв 2027.xlsx', 'document', 'Desktop'], ['Хурлын бичлэг.mp4', 'video', 'Videos'], ['logo_final.png', 'image', 'Desktop/MetaDesk'], ['backup.zip', 'archive', ''], ['дуу.mp3', 'audio', 'Music'], ['notes.txt', 'document', 'Desktop'], ['f000123456.jpg', 'image', 'Гүн скан/JPG']];
    if (!this.files.length) {
      for (let i = 0; i < 180; i++) {
        const [n, c, p] = names[i % names.length];
        const q = ['good', 'good', 'good', 'unknown', 'poor'][i % 5];
        this.files.push({ id: i + 1, name: i < names.length ? n : n.replace('.', `_${i}.`), path: p, size: 1024 * (37 + ((i * 7919) % 90000)), ext: n.split('.').pop(), category: c, source: p.startsWith('Гүн') ? 'carved' : 'ntfs', deleted: i % 6 !== 0, modified: 1789000000 + i * 86400, quality: q, offset: 0x1a2b000 + i * 4096, fragments: 1 + (i % 4 === 0 ? 3 : 0) });
      }
    }
    const filt = (f) => this.files.filter((x) => (!f.deletedOnly || x.deleted) && (f.category === 'all' || x.category === f.category) && (!f.quality || x.quality === f.quality) && (!f.search || (x.name + x.path).toLowerCase().includes(f.search.toLowerCase())));
    switch (cmd) {
      case 'is_elevated': return true;
      case 'list_devices': return [
        { path: '\\\\.\\C:', name: 'C: диск', size: 511e9, kind: 'volume', filesystem: 'NTFS', label: null },
        { path: '\\\\.\\D:', name: 'D: диск', size: 1.0e12, kind: 'volume', filesystem: 'NTFS', label: null },
        { path: '\\\\.\\E:', name: 'E: диск', size: 31.9e9, kind: 'volume', filesystem: 'FAT32', label: 'KINGSTON' },
        { path: '\\\\.\\F:', name: 'F: диск', size: 64e9, kind: 'volume', filesystem: null, label: null },
        { path: '\\\\.\\PhysicalDrive0', name: 'Физик диск 0', size: 512.1e9, kind: 'disk', filesystem: null, label: null },
      ];
      case 'open_image': return { path: a.path, name: 'disk.img', size: 64e6, kind: 'image', filesystem: 'FAT32', label: null };
      case 'start_scan': {
        let done = 0;
        const iv = setInterval(() => {
          done += 7;
          this.emit('scan-progress', { phase: done < 40 ? 'fs' : 'carve', message: '', done: Math.min(done, 100), total: 100, found: done * 3 });
          if (done >= 100) { clearInterval(iv); this.emit('scan-done', { total: 180, deleted: 150, volumes: [{ filesystem: 'NTFS', label: null }], errors: [], cancelled: false, seconds: 12.4 }); }
        }, 120);
        return;
      }
      case 'summary': { const s = { categories: {}, total: 0 }; this.files.filter((x) => !a.deletedOnly || x.deleted).forEach((x) => { s.categories[x.category] = (s.categories[x.category] || 0) + 1; s.total++; }); return s; }
      case 'query': { const v = filt(a.filter); return { total: v.length, total_bytes: v.reduce((s, x) => s + x.size, 0), items: v.slice(a.offset, a.offset + a.limit) }; }
      case 'preview': return { kind: 'hex', mime: '', data: '0000  FF D8 FF E0 00 10 4A 46 49 46 00 01 01 00 00 01  ......JFIF......\n0010  00 01 00 00 FF DB 00 43 00 08 06 06 07 06 05 08  .......C........' };
      case 'check_destination': return a.outDir.startsWith('C');
      case 'start_recover': { let d = 0; const t = 20; const iv = setInterval(() => { this.emit('recover-progress', [d, t, 'file_' + d + '.jpg']); if (++d > t) { clearInterval(iv); this.emit('recover-done', { ok: t, failed: 0, bytes: 54e6, errors: [], out_dir: a.outDir }); } }, 60); return; }
      default: return null;
    }
  },
};

loadDevices();
