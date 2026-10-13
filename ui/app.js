/* CUEd desktop UI: plain JS against cuedd's /api. No framework, no build step. */
'use strict';

const $ = (s, r = document) => r.querySelector(s);
const $$ = (s, r = document) => [...r.querySelectorAll(s)];
const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const fmtTime = s => { s = Math.max(0, Math.floor(s || 0)); return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`; };
const fmtBytes = b => { if (!b) return '0 B'; const u = ['B', 'KB', 'MB', 'GB', 'TB']; const i = Math.min(u.length - 1, Math.floor(Math.log(b) / Math.log(1024))); return `${(b / 1024 ** i).toFixed(i ? 1 : 0)} ${u[i]}`; };
const ago = ms => { if (!ms) return 'never'; const d = Date.now() - ms; if (d < 60e3) return 'just now'; if (d < 3600e3) return `${Math.floor(d / 60e3)} min ago`; if (d < 86400e3) return `${Math.floor(d / 3600e3)} h ago`; return new Date(ms).toLocaleDateString(); };
const shortKey = k => k ? `${k.slice(0, 10)}…${k.slice(-6)}` : '';

async function api(path, opts = {}) {
  const init = { method: opts.method || (opts.body !== undefined ? 'POST' : 'GET'), headers: {} };
  if (opts.body !== undefined) { init.headers['Content-Type'] = 'application/json'; init.body = JSON.stringify(opts.body); }
  const r = await fetch(path, init);
  const text = await r.text();
  let data = null; try { data = text ? JSON.parse(text) : null; } catch { data = text; }
  if (!r.ok) { const e = new Error((data && data.error) || `${r.status} ${r.statusText}`); e.status = r.status; throw e; }
  return data;
}
function toast(msg, isErr) { const d = document.createElement('div'); d.textContent = msg; if (isErr) d.style.borderColor = 'var(--danger)'; $('#toast').appendChild(d); setTimeout(() => d.remove(), 6000); }
const guard = fn => (...a) => fn(...a).catch(e => toast(e.message, true));
function kv(obj) { return `<dl class="kv">${Object.entries(obj).map(([k, v]) => `<dt>${esc(k)}</dt><dd>${v}</dd>`).join('')}</dl>`; }
async function copyText(t) { try { await navigator.clipboard.writeText(t); toast('Copied'); } catch { prompt('Copy:', t); } }

/* ---------- routing: #tab or #settings/<sub> ---------- */
const TABS = ['link', 'library', 'backup', 'stations', 'friends', 'relay', 'downloads', 'settings'];
const SUBS = ['general', 'theme', 'account', 'relay'];
const loaders = {}, subLoaders = {};
let currentTab = 'link', currentSub = 'general';
$$('#main-nav button').forEach(b => b.addEventListener('click', () => showTab(b.dataset.tab)));
$$('#settings-nav button').forEach(b => b.addEventListener('click', () => showTab('settings', b.dataset.sub)));
function showTab(name, sub) {
  const h = name === 'settings' ? `settings/${sub || currentSub}` : name;
  if (location.hash === `#${h}`) route(); else location.hash = h;
}
function route() {
  const [t, s] = location.hash.slice(1).split('/');
  currentTab = TABS.includes(t) ? t : 'link';
  if (currentTab === 'settings') currentSub = SUBS.includes(s) ? s : 'general';
  $$('#main-nav button').forEach(b => b.classList.toggle('active', b.dataset.tab === currentTab));
  $$('.tab').forEach(x => x.classList.toggle('active', x.id === `tab-${currentTab}`));
  $$('#settings-nav button').forEach(b => b.classList.toggle('active', b.dataset.sub === currentSub));
  $$('.sub').forEach(x => x.classList.toggle('active', x.id === `settings-${currentSub}`));
  loaders[currentTab]?.();
}
window.addEventListener('hashchange', route);
const tabActive = name => currentTab === name;
const subActive = name => currentTab === 'settings' && currentSub === name;

/* ---------- shared settings (account chain): theme + switches ---------- */
const THEME_KEYS = ['bg', 'panel', 'panel2', 'text', 'muted', 'accent', 'accent2', 'danger', 'border'];
const THEME_LABELS = { bg: 'Background', panel: 'Panel', panel2: 'Panel (raised)', text: 'Text', muted: 'Muted text', accent: 'Accent', accent2: 'Accent 2 / code', danger: 'Danger', border: 'Border' };
const DEFAULT_THEME = { bg: '#0e0f13', panel: '#161820', panel2: '#1d2029', text: '#ececf1', muted: '#8d92a3', accent: '#5ef2c6', accent2: '#7aa2ff', danger: '#ff6b7a', border: '#262a36' };
const PRESETS = {
  'Default dark': DEFAULT_THEME,
  'Light': { bg: '#f4f5f8', panel: '#ffffff', panel2: '#eaedf3', text: '#16181f', muted: '#5d6273', accent: '#0f9d78', accent2: '#3b5bdb', danger: '#d6334a', border: '#d8dbe3' },
  'High contrast': { bg: '#000000', panel: '#000000', panel2: '#141414', text: '#ffffff', muted: '#d4d4d4', accent: '#ffe600', accent2: '#00e5ff', danger: '#ff4040', border: '#ffffff' },
  'Warm': { bg: '#1a1410', panel: '#241c16', panel2: '#2f251c', text: '#f3e9dc', muted: '#a89a88', accent: '#f0a35e', accent2: '#e76f51', danger: '#ff6b6b', border: '#3b2e24' },
};
let shared = { relayEnabled: false, friendStreaming: true, theme: { ...DEFAULT_THEME }, stationName: '' };
let themeDirty = false;
const themeColors = { accent: DEFAULT_THEME.accent, accent2: DEFAULT_THEME.accent2 };
const lum = hex => { const m = /^#?([0-9a-f]{6})$/i.exec(hex || ''); if (!m) return 0; const n = parseInt(m[1], 16); const c = [16, 8, 0].map(sh => { const v = ((n >> sh) & 255) / 255; return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; }); return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]; };
function applyTheme(t) {
  const root = document.documentElement.style;
  const full = { ...DEFAULT_THEME, ...(t || {}) };
  for (const k of THEME_KEYS) root.setProperty(`--${k}`, full[k]);
  // text on the accent: whichever of bg / text contrasts more with it, so every colour stays a theme colour
  const la = lum(full.accent);
  root.setProperty('--on-accent', Math.abs(la - lum(full.bg)) >= Math.abs(la - lum(full.text)) ? 'var(--bg)' : 'var(--text)');
  themeColors.accent = full.accent; themeColors.accent2 = full.accent2;
}
function setShared(s) {
  if (!s) return;
  shared = { ...shared, ...s, theme: { ...DEFAULT_THEME, ...(s.theme || shared.theme) } };
  if (!themeDirty) { applyTheme(shared.theme); fillThemeInputs(shared.theme); }
  renderSharedToggles();
}
function renderSharedToggles() {
  for (const el of $$('[data-shared]')) el.checked = !!shared[el.dataset.shared];
  const badge = $('#relay-enabled-badge'); if (badge) { badge.textContent = shared.relayEnabled ? 'on' : 'off'; badge.classList.toggle('on', !!shared.relayEnabled); }
}
const loadShared = guard(async () => setShared(await api('/api/settings/shared')));
async function putShared(patch) { setShared(await api('/api/settings/shared', { method: 'PUT', body: patch })); }
$$('[data-shared]').forEach(el => el.addEventListener('change', guard(async e => {
  const key = e.target.dataset.shared, val = e.target.checked;
  try { await putShared({ [key]: val }); toast(key === 'relayEnabled' ? `Desktop relay ${val ? 'on' : 'off'} on every device of this account` : `Friend streaming ${val ? 'on' : 'off'}`); }
  catch (err) { renderSharedToggles(); throw err; }
})));

/* theme page */
function inputsTheme() { const t = {}; for (const el of $$('#theme-inputs input')) t[el.dataset.key] = el.value; return t; }
function fillThemeInputs(t) { for (const el of $$('#theme-inputs input')) { el.value = t[el.dataset.key] || DEFAULT_THEME[el.dataset.key]; el.nextElementSibling.lastElementChild.textContent = el.value; } }
function setThemeDirty(d) { themeDirty = d; $('#theme-state').textContent = d ? 'Unsaved changes: previewing' : ''; }
(function buildThemePage() {
  $('#theme-inputs').innerHTML = THEME_KEYS.map(k => `<label class="swatch"><input type="color" data-key="${k}" value="${DEFAULT_THEME[k]}"><span><span class="k block">${esc(THEME_LABELS[k])}</span><code>${DEFAULT_THEME[k]}</code></span></label>`).join('');
  $('#theme-inputs').addEventListener('input', e => { e.target.nextElementSibling.lastElementChild.textContent = e.target.value; setThemeDirty(true); applyTheme(inputsTheme()); });
  $('#theme-presets').innerHTML = Object.entries(PRESETS).map(([n, t]) => `<button class="preset" data-preset="${esc(n)}"><span class="dots">${['bg', 'panel2', 'text', 'accent', 'accent2'].map(k => `<i style="background:${t[k]}"></i>`).join('')}</span>${esc(n)}</button>`).join('');
  $('#theme-presets').addEventListener('click', e => { const b = e.target.closest('[data-preset]'); if (!b) return; fillThemeInputs(PRESETS[b.dataset.preset]); setThemeDirty(true); applyTheme(inputsTheme()); });
})();
$('#theme-save').addEventListener('click', guard(async () => { const t = inputsTheme(); setThemeDirty(false); await putShared({ theme: t }); toast('Theme saved; the phones of this account pick it up'); }));
$('#theme-revert').addEventListener('click', () => { setThemeDirty(false); applyTheme(shared.theme); fillThemeInputs(shared.theme); });
$('#theme-reset').addEventListener('click', guard(async () => { setThemeDirty(false); await putShared({ theme: { ...DEFAULT_THEME } }); toast('Theme reset to the default'); }));
subLoaders.theme = () => { if (!themeDirty) fillThemeInputs(shared.theme); };

/* ---------- status / settings ---------- */
let status = {};
async function loadStatus() {
  status = await api('/api/status');
  $('#desktop-name').textContent = status.name;
  $('#relay-url').textContent = status.relay; $('#relay-url2').textContent = status.relay;
  $('#data-dir').textContent = status.dataDir;
  $('#about').innerHTML = kv({ Version: status.version, 'Data directory': status.dataDir, 'LAN addresses': (status.lan || []).join(', ') || 'none found', 'Audio engine': status.audio?.enabled ? `on (${status.audio.version}${status.audio.headless ? ', headless' : ''})` : 'off', 'Library': `${status.library.tracks} tracks, ${fmtBytes(status.library.bytes)}`, 'Uptime': fmtTime(status.uptimeSec), 'Account': status.account ? `${esc(status.account.name || '(no name)')} · ${esc(shortKey(status.account.npub))} · ${status.account.devices} device${status.account.devices === 1 ? '' : 's'}` : 'none' });
  if (status.shared) setShared(status.shared);
  if (status.listening) listening = status.listening;
}

loaders.settings = guard(async () => { await loadStatus(); await subLoaders[currentSub]?.(); });
subLoaders.general = async () => {
  const s = await api('/api/settings');
  const f = $('#settings-form');
  for (const el of f.elements) if (el.name && s[el.name] !== undefined) el.value = s[el.name];
};
$('#settings-form').addEventListener('submit', guard(async e => {
  e.preventDefault();
  const f = e.target; const body = {};
  for (const el of f.elements) if (el.name) body[el.name] = el.type === 'number' || el.name === 'crossfade_curve' ? Number(el.value) : el.value;
  await api('/api/settings', { method: 'PUT', body });
  toast('Saved'); loadStatus();
}));

/* account page */
let account = null;
subLoaders.account = async () => {
  account = await api('/api/account');
  $('#acct-npub').textContent = account.npub || '';
  if (document.activeElement !== $('#acct-name')) $('#acct-name').value = account.name || '';
  $('#acct-height').textContent = `${account.height ?? 0} block${account.height === 1 ? '' : 's'}`;
  $('#acct-tip').textContent = account.tip || '';
  const el = $('#acct-devices'); el.innerHTML = (account.devices || []).length ? '' : '<div class="empty">No devices bound yet.</div>';
  for (const d of account.devices || []) {
    const mine = d.devhash === account.thisDevice;
    const row = document.createElement('div'); row.className = 'item';
    row.innerHTML = `<div class="grow"><div class="title">${esc(d.name || d.kind || 'device')} <span class="badge">${esc(d.kind || '')}</span>${mine ? ' <span class="badge on">this desktop</span>' : ''}</div><div class="sub">${esc(shortKey(d.devhash))} · bound ${ago(d.boundAt)}</div></div>${mine ? '' : '<button class="danger">Unbind</button>'}`;
    row.querySelector('button')?.addEventListener('click', guard(async () => { if (confirm(`Unbind ${d.name || 'this device'} from the account?`)) { await api('/api/account/unbind', { body: { devhash: d.devhash } }); await subLoaders.account(); } }));
    el.appendChild(row);
  }
  $('#acct-nsec').hidden = true; $('#acct-reveal').textContent = 'Reveal private key';
};
$('#acct-copy').addEventListener('click', () => copyText($('#acct-npub').textContent));
$('#acct-name-form').addEventListener('submit', guard(async e => { e.preventDefault(); await api('/api/account/profile', { body: { name: $('#acct-name').value.trim() } }); toast('Name updated'); await subLoaders.account(); loadStatus(); }));
$('#acct-reveal').addEventListener('click', guard(async () => {
  const code = $('#acct-nsec');
  if (!code.hidden) { code.hidden = true; code.textContent = ''; $('#acct-reveal').textContent = 'Reveal private key'; return; }
  if (!confirm('Show the private key on screen? Anyone who sees it can act as this account. Only the public key is ever visible to others.')) return;
  const r = await api('/api/account/export?reveal=true');
  code.textContent = `${r.nsec}\n(chain: ${Array.isArray(r.chain) ? r.chain.length : (r.chain?.length ?? '?')} blocks)`; code.hidden = false; $('#acct-reveal').textContent = 'Hide private key';
}));
$('#acct-new').addEventListener('click', guard(async () => {
  const name = prompt('Name for the new account (shown to friends):', account?.name || '');
  if (name === null) return;
  if (!confirm('Start a new account? A fresh key pair replaces the current one. The old key is backed up on disk, but friends and phones bound to it will not follow automatically.')) return;
  await api('/api/account/new', { body: { name: name.trim() } });
  toast('New account created'); await subLoaders.account(); loadStatus();
}));

/* relay & privacy page */
subLoaders.relay = async () => {
  renderSharedToggles();
  const r = await api('/api/relay/stats').catch(() => null);
  $('#relay-mini-stats').innerHTML = r ? kv({ 'Relay': (r.enabled ?? shared.relayEnabled) ? '<span class="badge on">on</span>' : '<span class="badge">off</span>', 'Address': esc(r.url || ''), 'Stored events': r.stats?.events ?? 0, 'Open connections': r.stats?.connections ?? 0 }) : '';
};

/* ---------- link a phone ---------- */
function bindPairButton() { $('#pair-qr button')?.addEventListener('click', guard(async () => {
  const r = await api('/api/pair/new', { body: {} });
  $('#pair-qr').innerHTML = r.svg;
  $('#pair-link').textContent = r.link;
  $('#pair-addrs').textContent = `Addresses in the code: ${r.addresses.join(', ') || 'none (is this machine on a network?)'}${r.funnel ? ` · funnel ${r.funnel}` : ''} · expires in 10 minutes`;
})); }
bindPairButton();
async function loadDevices() {
  const devs = await api('/api/devices');
  const el = $('#devices');
  el.innerHTML = devs.length ? '' : '<div class="empty">No phones linked yet.</div>';
  for (const d of devs) {
    const row = document.createElement('div'); row.className = 'item';
    row.innerHTML = `<div class="grow"><div class="title">${esc(d.device.name)} <span class="badge">${esc(d.device.id)}</span></div><div class="sub">${d.onPhone} on phone · ${d.backedUp} backed up (${fmtBytes(d.bytes)}) · seen ${ago(d.device.last_seen)}</div></div><button class="danger" data-id="${esc(d.device.id)}">Unlink</button>`;
    row.querySelector('button').addEventListener('click', guard(async e => { if (confirm(`Unlink ${d.device.name}?`)) { await api(`/api/devices/${e.target.dataset.id}`, { method: 'DELETE' }); loadDevices(); } }));
    el.appendChild(row);
  }
  const sel = $('#lib-device'); const cur = sel.value;
  sel.innerHTML = '<option value="">All sources</option>' + devs.map(d => `<option value="${esc(d.device.id)}">${esc(d.device.name)}</option>`).join('');
  sel.value = cur;
  return devs;
}
loaders.link = guard(loadDevices);
$('#receive-form').addEventListener('submit', guard(async e => {
  e.preventDefault();
  $('#receive-result').textContent = 'Pulling…';
  const r = await api('/api/receive', { body: { text: $('#receive-text').value } });
  $('#receive-result').textContent = r.track ? `Added: ${r.track.artist} – ${r.track.title}` : r.status;
  $('#receive-text').value = '';
}));

/* ---------- library ---------- */
let libTimer;
$('#lib-search').addEventListener('input', () => { clearTimeout(libTimer); libTimer = setTimeout(loaders.library, 200); });
$('#lib-device').addEventListener('change', () => loaders.library());
loaders.library = guard(async () => {
  const q = encodeURIComponent($('#lib-search').value);
  const dev = encodeURIComponent($('#lib-device').value);
  const r = await api(`/api/library?q=${q}&device=${dev}&limit=500`);
  $('#lib-count').textContent = `${r.total} track${r.total === 1 ? '' : 's'}`;
  const tb = $('#lib-table tbody'); tb.innerHTML = '';
  if (!r.tracks.length) { tb.innerHTML = '<tr><td colspan="8" class="empty">Nothing here yet. Pair a phone and back it up, pull a share, or import a folder.</td></tr>'; return; }
  for (const t of r.tracks) {
    const tr = document.createElement('tr');
    tr.innerHTML = `<td><button class="icon" title="Play" data-act="play">▶</button><button class="icon" title="Add to queue" data-act="queue">＋</button></td><td>${esc(t.title)}</td><td>${esc(t.artist)}</td><td>${esc(t.album)}</td><td class="num">${t.bpm ? t.bpm.toFixed(0) : '<span class="muted">…</span>'}</td><td class="num">${t.durationMs ? fmtTime(t.durationMs / 1000) : ''}</td><td>${t.devices.length ? `<span class="badge on">${t.devices.length} phone${t.devices.length > 1 ? 's' : ''}</span>` : `<span class="badge">${esc(t.source)}</span>`}</td><td><button class="icon" title="Send to a phone (QR)" data-act="share">⇪</button><a class="icon" title="Open file" href="/api/track/${t.sha256}" target="_blank">⤓</a></td>`;
    tr.addEventListener('click', guard(async e => {
      const act = e.target.dataset?.act; if (!act) return;
      if (act === 'play') { await api('/api/player/load', { body: { sha256: t.sha256 } }); pollPlayer(); }
      if (act === 'queue') { await api('/api/player/queue', { body: { sha256: t.sha256 } }); toast(`Queued ${t.title}`); }
      if (act === 'share') await showShare(t.sha256);
    }));
    tb.appendChild(tr);
  }
});
async function showShare(sha) {
  const node = $('#share-dialog').content.cloneNode(true);
  const modal = node.querySelector('.modal'), cb = node.querySelector('.share-friend');
  node.querySelector('.close').addEventListener('click', () => modal.remove());
  modal.addEventListener('click', e => { if (e.target === modal) modal.remove(); });
  const load = guard(async () => {
    modal.classList.add('busy');
    try { const r = await api(`/api/share/${sha}`, { body: { friend: cb.checked } }); modal.querySelector('.qr').innerHTML = r.svg; modal.querySelector('code').textContent = r.link; }
    finally { modal.classList.remove('busy'); }
  });
  cb.addEventListener('change', load);
  document.body.appendChild(node);
  await load();
}
$('#import-form').addEventListener('submit', guard(async e => {
  e.preventDefault();
  const r = await api('/api/library/import', { body: { path: $('#import-path').value } });
  toast(`Imported ${r.added}, skipped ${r.skipped}`); loaders.library();
}));

/* ---------- backup ---------- */
loaders.backup = guard(async () => {
  const devs = await loadDevices();
  const el = $('#backup-cards'); el.innerHTML = devs.length ? '' : '<div class="card empty">Link a phone first.</div>';
  for (const d of devs) {
    const pct = d.onPhone ? Math.round(100 * (d.onPhone - d.missing) / d.onPhone) : 0;
    const c = document.createElement('div'); c.className = 'card';
    c.innerHTML = `<h2>${esc(d.device.name)}</h2>${kv({ 'On the phone': d.onPhone, 'Backed up here': `${d.backedUp} (${fmtBytes(d.bytes)})`, 'Still to upload': d.missing, 'Last manifest': ago(d.device.last_manifest), 'Last upload': ago(d.device.last_upload) })}<div class="bar"><i style="width:${pct}%"></i></div><p class="muted small">${pct}% of what is on the phone is here.</p>`;
    el.appendChild(c);
  }
});

/* ---------- listening in (shared by Stations and Friends) ---------- */
let listening = { listening: null, status: 'idle' };
async function loadListening() { try { listening = await api('/api/stations/listen'); } catch { listening = { listening: null, status: 'idle' }; } return listening; }
function renderListen(el) {
  if (!el) return;
  if (!listening?.listening) { el.innerHTML = ''; return; }
  const now = listening.now ? `${esc(listening.now.artist || '')} – ${esc(listening.now.title || '')}` : 'waiting for a track';
  const badge = listening.status === 'playing' ? 'on' : listening.status === 'error' ? 'warn' : '';
  el.innerHTML = `<div class="grow"><div class="title">Listening to ${esc(listening.name || shortKey(listening.listening))} <span class="badge ${badge}">${esc(listening.status || '')}</span></div><div class="sub">${now}${listening.driftMs ? ` · drift ${Math.round(listening.driftMs)} ms` : ''}${listening.message ? ` · ${esc(listening.message)}` : ''}</div></div><button class="danger">Stop listening</button>`;
  el.querySelector('button').addEventListener('click', guard(stopListening));
}
async function startListening(pubkey) { await api('/api/stations/listen', { body: { pubkey } }); await loadListening(); refreshSocial(); }
async function stopListening() { await api('/api/stations/listen', { method: 'DELETE' }); await loadListening(); refreshSocial(); }
function listenButton(pubkey) {
  const b = document.createElement('button');
  const on = listening?.listening === pubkey;
  b.textContent = on ? 'Stop listening' : 'Listen'; b.className = on ? 'danger' : 'primary';
  b.addEventListener('click', guard(async () => on ? stopListening() : startListening(pubkey)));
  return b;
}
function refreshSocial() { if (tabActive('stations')) loaders.stations(); if (tabActive('friends')) loaders.friends(); }

/* ---------- stations ---------- */
let hosting = false;
loaders.stations = guard(async () => {
  const [feed, link, host] = await Promise.all([api('/api/stations/feed'), api('/api/stations/link'), api('/api/stations/host'), loadListening()]);
  renderListen($('#listen-now-stations'));
  hosting = feed.hosting;
  $('#host-toggle').textContent = hosting ? 'Stop' : 'Start';
  $('#host-toggle').classList.toggle('primary', !hosting);
  if (!$('#station-name').value) $('#station-name').value = host.name || '';
  $('#station-qr').innerHTML = link.svg; $('#station-npub').textContent = `${link.npub}\n${link.link}`;
  const el = $('#stations-feed'); el.innerHTML = '';
  const follows = await api('/api/stations/follows');
  const byPk = Object.fromEntries(feed.stations.map(s => [s.pubkey, s]));
  const rows = follows.map(f => ({ f, s: byPk[f.pubkey] })).concat(feed.stations.filter(s => s.mine).map(s => ({ f: null, s })));
  if (!rows.length) el.innerHTML = '<div class="empty">Not following anyone. Paste a cued://station link from a phone\'s "Your station" screen.</div>';
  for (const { f, s } of rows) {
    const row = document.createElement('div'); row.className = 'item';
    const st = s?.state;
    const now = st?.now ? `${esc(st.now.artist)} – ${esc(st.now.title)}` : (st ? 'nothing playing' : 'no state seen yet');
    const live = st?.status === 'on_air' ? `<span class="badge on">on air · ${fmtTime(s.expectedPositionMs / 1000)}</span>` : `<span class="badge">${esc(st?.status || 'quiet')}</span>`;
    row.innerHTML = `<div class="grow"><div class="title">${esc(f?.name || st?.name || s?.npub?.slice(0, 16) || '')}${s?.mine ? ' <span class="badge">this desktop</span>' : ''}${s?.friend ? ' <span class="badge alt">friend</span>' : ''} ${live}</div><div class="sub">${now}${st?.next?.length ? ` · next: ${esc(st.next[0].artist)} – ${esc(st.next[0].title)}` : ''}${s ? ` · via ${esc(s.via)}` : ''}</div></div>`;
    if (s?.canListen && !s.mine) row.appendChild(listenButton(s.pubkey));
    if (f) { const b = document.createElement('button'); b.className = 'danger'; b.textContent = 'Unfollow'; b.addEventListener('click', guard(async () => { await api(`/api/stations/follows/${f.pubkey}`, { method: 'DELETE' }); loaders.stations(); })); row.appendChild(b); }
    el.appendChild(row);
  }
});
$('#follow-form').addEventListener('submit', guard(async e => { e.preventDefault(); await api('/api/stations/follows', { body: { text: $('#follow-text').value } }); $('#follow-text').value = ''; loaders.stations(); }));
$('#host-toggle').addEventListener('click', guard(async () => { await api('/api/stations/host', { body: { on: !hosting, name: $('#station-name').value } }); loaders.stations(); }));
$('#reveal-nsec').addEventListener('click', guard(async () => { const r = await api('/api/identity?reveal=true'); $('#nsec').textContent = r.nsec; }));

/* ---------- friends ---------- */
const FRIEND_STATUS = { friends: ['friends', 'on'], pending_out: ['request sent', ''], pending_in: ['wants to be friends', 'alt'] };
loaders.friends = guard(async () => {
  const [friends, requests, link, feed] = await Promise.all([api('/api/friends'), api('/api/friends/requests'), api('/api/friends/link'), api('/api/stations/feed').catch(() => ({ stations: [] })), loadListening()]);
  renderListen($('#listen-now-friends'));
  $('#friend-qr').innerHTML = link.svg || ''; $('#friend-link').textContent = `${link.npub}\n${link.link}`;
  const byPk = Object.fromEntries((feed.stations || []).map(s => [s.pubkey, s]));
  const reqEl = $('#friend-requests'); reqEl.innerHTML = requests.length ? '' : '<div class="empty">No pending requests.</div>';
  for (const r of requests) {
    const row = document.createElement('div'); row.className = 'item';
    row.innerHTML = `<div class="grow"><div class="title">${esc(r.name || shortKey(r.npub))}</div><div class="sub">${esc(r.npub)} · ${ago(r.updatedAt || r.addedAt)}</div></div><button class="primary">Accept</button><button class="danger">Decline</button>`;
    const [acc, dec] = row.querySelectorAll('button');
    acc.addEventListener('click', guard(async () => { await api(`/api/friends/${r.pubkey}/accept`, { body: {} }); toast(`You and ${r.name || 'they'} are friends`); loaders.friends(); }));
    dec.addEventListener('click', guard(async () => { await api(`/api/friends/${r.pubkey}`, { method: 'DELETE' }); loaders.friends(); }));
    reqEl.appendChild(row);
  }
  const list = friends.filter(f => f.status !== 'pending_in');
  const el = $('#friends-list'); el.innerHTML = list.length ? '' : '<div class="empty">No friends yet. Paste a friend link or an npub on the left, or scan this desktop\'s code with a phone.</div>';
  for (const f of list) {
    const s = byPk[f.pubkey], st = s?.state;
    const [label, cls] = FRIEND_STATUS[f.status] || [f.status, ''];
    const onAir = st?.status === 'on_air';
    const now = onAir && st.now ? `on air: ${esc(st.now.artist)} – ${esc(st.now.title)}` : (f.status === 'friends' ? 'not playing right now' : 'waiting for them to accept');
    const row = document.createElement('div'); row.className = 'item';
    row.innerHTML = `<div class="grow"><div class="title">${esc(f.name || shortKey(f.npub))} <span class="badge ${cls}">${esc(label)}</span>${onAir ? ' <span class="badge on">on air</span>' : ''}</div><div class="sub">${esc(shortKey(f.npub))} · ${now}</div></div>`;
    if (f.status === 'friends' && (s?.canListen || listening?.listening === f.pubkey)) row.appendChild(listenButton(f.pubkey));
    const rm = document.createElement('button'); rm.className = 'danger'; rm.textContent = f.status === 'friends' ? 'Remove' : 'Cancel';
    rm.addEventListener('click', guard(async () => { if (f.status !== 'friends' || confirm(`Remove ${f.name || 'this friend'}?`)) { await api(`/api/friends/${f.pubkey}`, { method: 'DELETE' }); loaders.friends(); } }));
    row.appendChild(rm);
    el.appendChild(row);
  }
});
$('#friend-form').addEventListener('submit', guard(async e => { e.preventDefault(); await api('/api/friends', { body: { text: $('#friend-text').value.trim() } }); $('#friend-text').value = ''; toast('Friend request sent'); loaders.friends(); }));

/* ---------- relay & funnel ---------- */
loaders.relay = guard(async () => {
  const [r, s, f] = await Promise.all([api('/api/relay/stats'), api('/api/settings'), api('/api/funnel')]);
  renderSharedToggles();
  $('#relay-url').textContent = r.url;
  $('#relay-stats').innerHTML = kv({ 'Stored events': r.stats.events, 'Open connections': r.stats.connections, 'Subscriptions': r.stats.subscriptions });
  const up = $('#upstream'); up.innerHTML = s.upstream_relays.length ? '' : '<div class="empty">No upstream relays: the desktop relay stands alone.</div>';
  for (const u of s.upstream_relays) {
    const row = document.createElement('div'); row.className = 'item';
    row.innerHTML = `<div class="grow"><div class="title">${esc(u)}</div></div><span class="badge ${r.upstream[u] ? 'on' : ''}">${r.upstream[u] ? 'connected' : 'offline'}</span><button class="danger">Remove</button>`;
    row.querySelector('button').addEventListener('click', guard(async () => { await api('/api/settings', { method: 'PUT', body: { upstream_relays: s.upstream_relays.filter(x => x !== u) } }); loaders.relay(); }));
    up.appendChild(row);
  }
  $('#mirror-out').checked = s.mirror_out; $('#mirror-in').checked = s.mirror_in;
  const st = f.status || {};
  $('#funnel').innerHTML = f.url ? kv({ 'Public address': f.url, 'Certificate': `<code>${esc(f.cert)}</code>`, 'Edge': st.connected ? '<span class="badge on">connected</span>' : '<span class="badge">not connected</span>', 'Streams': `${st.streams_open ?? 0} open · ${st.streams_total ?? 0} total`, 'Traffic': `${fmtBytes(st.bytes_in)} in · ${fmtBytes(st.bytes_out)} out`, 'Last error': esc(st.last_error || 'none') }) : '<p class="muted">Not configured. Start cuedd with <code>--funnel</code> to reach this desktop from outside the house.</p>';
});
$('#upstream-form').addEventListener('submit', guard(async e => { e.preventDefault(); const s = await api('/api/settings'); const u = $('#upstream-url').value.trim(); if (!/^wss?:\/\//.test(u)) throw new Error('relay URLs start with wss://'); await api('/api/settings', { method: 'PUT', body: { upstream_relays: [...s.upstream_relays, u] } }); $('#upstream-url').value = ''; loaders.relay(); }));
$('#mirror-out').addEventListener('change', guard(async e => { await api('/api/settings', { method: 'PUT', body: { mirror_out: e.target.checked } }); }));
$('#mirror-in').addEventListener('change', guard(async e => { await api('/api/settings', { method: 'PUT', body: { mirror_in: e.target.checked } }); }));
$('#companion-open').addEventListener('change', guard(async e => { await api('/api/settings', { method: 'PUT', body: { companion_open_lan: e.target.checked } }); }));

/* ---------- downloads ---------- */
loaders.downloads = guard(async () => {
  await loadStatus();
  const c = status.companion || {};
  try { const s = await api('/api/settings'); $('#companion-open').checked = !!s.companion_open_lan; } catch (_) {}
  $('#companion').innerHTML = kv({ Worker: c.running ? '<span class="badge on">running</span>' : `<span class="badge">off</span> ${esc(c.error || '')}`, Python: esc(c.python || ''), 'Imported into library': c.imported ?? 0 });
  const el = $('#jobs');
  if (!c.running) { el.innerHTML = ''; return; }
  const jobs = await api('/jobs');
  const list = Array.isArray(jobs) ? jobs : (jobs.jobs || []);
  el.innerHTML = list.length ? '' : '<div class="empty">No jobs.</div>';
  for (const j of list.slice().reverse()) {
    const row = document.createElement('div'); row.className = 'item';
    row.innerHTML = `<div class="grow"><div class="title">${esc(j.url)} <span class="badge ${j.status === 'done' ? 'on' : ''}">${esc(j.status)}</span></div><div class="sub">${esc(j.message || '')}</div><div class="bar"><i style="width:${Math.round((j.progress || 0) * 100)}%"></i></div></div>`;
    el.appendChild(row);
  }
});
$('#job-form').addEventListener('submit', guard(async e => { e.preventDefault(); await api('/jobs', { body: { url: $('#job-url').value, lrc: false } }); $('#job-url').value = ''; toast('Queued'); setTimeout(loaders.downloads, 500); }));

/* ---------- mini player: cover, spectrograph, lyrics ---------- */
const mini = $('#mini');
const canvas = $('#spectrum'); const ctx = canvas.getContext('2d');
let seeking = false, volDragging = false, curSha = null, lastPos = 0, lyrics = null, expanded = false;
function drawSpectrum(bands) {
  const w = canvas.width, h = canvas.height; ctx.clearRect(0, 0, w, h);
  if (!bands || !bands.length) return;
  const bw = w / bands.length;
  for (let i = 0; i < bands.length; i++) { const v = Math.max(0, Math.min(1, bands[i])); ctx.fillStyle = v > 0.7 ? themeColors.accent2 : themeColors.accent; ctx.fillRect(i * bw, h - v * h, Math.max(1, bw - 1), v * h); }
}
function setCover(sha) {
  const img = $('#mini-cover');
  mini.classList.remove('has-cover');
  if (!sha) { img.removeAttribute('src'); return; }
  img.onload = () => { if (sha === curSha) mini.classList.add('has-cover'); };
  img.onerror = () => mini.classList.remove('has-cover');
  img.src = `/api/track/${sha}/cover`;
}
function parseLyrics(r) {
  const plain = () => ({ synced: false, lines: String(r.text || '').split(/\r?\n/).map(text => ({ t: null, text })) });
  if (!r.synced) return plain();
  const out = [];
  for (const raw of String(r.text || '').split(/\r?\n/)) {
    const m = /^((?:\[\d{1,3}:\d{2}(?:[.:]\d{1,3})?\])+)(.*)$/.exec(raw.trim()); if (!m) continue;
    const text = m[2].trim();
    for (const ts of m[1].matchAll(/\[(\d{1,3}):(\d{2})(?:[.:](\d{1,3}))?\]/g)) out.push({ t: Number(ts[1]) * 60 + Number(ts[2]) + (ts[3] ? Number(`0.${ts[3]}`) : 0), text });
  }
  if (!out.length) return plain();
  out.sort((a, b) => a.t - b.t);
  return { synced: true, lines: out };
}
async function loadLyrics() {
  const sha = curSha, box = $('#lyrics-inner'), wrap = $('#lyrics');
  wrap.classList.remove('static'); wrap.classList.add('msg'); box.style.transform = '';
  if (!sha) { lyrics = null; box.innerHTML = '<div class="ln">Nothing playing</div>'; return; }
  box.innerHTML = '<div class="ln">Loading lyrics…</div>';
  let r = null;
  try { r = await api(`/api/track/${sha}/lyrics`); } catch { r = null; }
  if (sha !== curSha) return;
  if (!r || !r.text) { lyrics = { synced: false, lines: [] }; box.innerHTML = '<div class="ln">No lyrics</div>'; return; }
  lyrics = parseLyrics(r);
  wrap.classList.remove('msg'); wrap.classList.toggle('static', !lyrics.synced);
  box.innerHTML = lyrics.synced ? lyrics.lines.map(l => `<div class="ln">${esc(l.text) || '♪'}</div>`).join('') : esc(lyrics.lines.map(l => l.text).join('\n'));
  if (lyrics.synced) syncLyrics(lastPos);
}
function syncLyrics(pos) {
  if (!expanded || !lyrics?.synced) return;
  const box = $('#lyrics-inner'), wrap = $('#lyrics'), els = box.children;
  let idx = -1; for (let i = 0; i < lyrics.lines.length; i++) { if (lyrics.lines[i].t <= pos) idx = i; else break; }
  for (let i = 0; i < els.length; i++) els[i].classList.toggle('cur', i === idx);
  const target = idx >= 0 ? els[idx] : els[0];
  if (!target) return;
  const centre = wrap.clientHeight / 2 - (target.offsetTop + target.offsetHeight / 2);
  box.style.transform = `translateY(${Math.min(0, Math.round(centre))}px)`;
}
function renderPlayer(st) {
  const cur = st.current; const sha = cur?.sha256 || null;
  if (sha !== curSha) { curSha = sha; setCover(sha); lyrics = null; if (expanded) loadLyrics(); }
  lastPos = st.positionSec || 0;
  $('#now-title').textContent = cur ? cur.title : (st.engine ? 'Nothing playing' : 'Audio engine off');
  $('#now-artist').textContent = cur ? `${cur.artist || ''}${st.crossfading ? ' · crossfading' : ''}` : '';
  $('#p-play').textContent = st.playing ? '❙❙' : '▶';
  if (!seeking) { const p = st.durationSec ? 1000 * st.positionSec / st.durationSec : 0; $('#p-seek').value = Math.round(p); $('#p-seek').style.setProperty('--p', `${p / 10}%`); }
  if (!volDragging && typeof st.volume === 'number') $('#p-vol').value = Math.round(st.volume * 100);
  $('#p-time').textContent = `${fmtTime(st.positionSec)} / ${fmtTime(st.durationSec)}`;
  $('#p-queue').textContent = st.queue ? `+${st.queue}` : '';
  if (expanded && lyrics?.synced) syncLyrics(lastPos);
}
$('#p-play').addEventListener('click', guard(async () => { const st = await api('/api/player'); renderPlayer(await api(st.playing ? '/api/player/pause' : '/api/player/play', { body: {} })); }));
$('#p-next').addEventListener('click', guard(async () => { renderPlayer(await api('/api/player/next', { body: {} })); }));
$('#p-seek').addEventListener('input', e => { seeking = true; e.target.style.setProperty('--p', `${e.target.value / 10}%`); });
$('#p-seek').addEventListener('change', guard(async e => { seeking = false; const st = await api('/api/player'); renderPlayer(await api('/api/player/seek', { body: { positionSec: st.durationSec * e.target.value / 1000 } })); }));
$('#p-vol').addEventListener('input', () => { volDragging = true; });
$('#p-vol').addEventListener('change', guard(async e => { volDragging = false; await api('/api/player/volume', { body: { volume: e.target.value / 100 } }); }));
$('#mini-lyrics').addEventListener('click', () => { expanded = !expanded; mini.classList.toggle('expanded', expanded); if (expanded) { if (!lyrics) loadLyrics(); else setTimeout(() => syncLyrics(lastPos), 260); } });
$('#mini-close').addEventListener('click', () => { mini.hidden = true; $('#mini-open').hidden = false; });
$('#mini-open').addEventListener('click', () => { mini.hidden = false; $('#mini-open').hidden = true; pollPlayer(); });

/* live events, with a slow poll while the socket is down so the page still works without it */
let wsOpen = false;
async function pollPlayer() { try { renderPlayer(await api('/api/player')); } catch { /* engine off or daemon away: keep the last state */ } }
setInterval(() => { if (!wsOpen && !mini.hidden) pollPlayer(); }, 3000);
function connectEvents() {
  let ws;
  try { ws = new WebSocket(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/ws/events`); } catch { setTimeout(connectEvents, 5000); return; }
  ws.onopen = () => { wsOpen = true; $('#live-dot').classList.add('on'); };
  ws.onclose = () => { wsOpen = false; $('#live-dot').classList.remove('on'); setTimeout(connectEvents, 2000); };
  ws.onmessage = ev => {
    let m; try { m = JSON.parse(ev.data); } catch { return; }
    switch (m.type) {
      case 'player': renderPlayer(m.status); drawSpectrum(m.spectrum); break;
      case 'devicePaired': toast(`${m.name} linked`); if (tabActive('link')) loaders.link(); $('#pair-qr').innerHTML = '<button id="pair-new" class="primary">Show a pairing code</button>'; bindPairButton(); break;
      case 'trackAdded': toast(`Added ${m.artist} – ${m.title}`); if (tabActive('library')) loaders.library(); if (tabActive('backup')) loaders.backup(); break;
      case 'station': if (tabActive('stations')) loaders.stations(); break;
      case 'relay': if (tabActive('relay')) loaders.relay(); break;
      case 'companion': toast(`Download ready: ${m.message}`); if (tabActive('downloads')) loaders.downloads(); break;
      case 'friends': if (tabActive('friends')) loaders.friends(); else if (m.status === 'pending_in' || m.request) toast(`Friend request from ${m.name || 'someone'}`); break;
      case 'shared': if (m.shared) setShared(m.shared); else loadShared(); if (subActive('relay')) subLoaders.relay(); break;
      case 'account': if (subActive('account')) subLoaders.account(); loadStatus().catch(() => {}); break;
      case 'listen': loadListening().then(() => { renderListen($('#listen-now-stations')); renderListen($('#listen-now-friends')); refreshSocial(); }); break;
      case 'notice': toast(m.text); break;
    }
  };
}

/* ---------- boot ---------- */
guard(async () => {
  await loadStatus();
  if (!status.shared) await loadShared();
  connectEvents();
  pollPlayer();
  route();
})();
