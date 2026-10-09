/* CUEd desktop UI: plain JS against cuedd's /api. No framework, no build step. */
'use strict';

const $ = (s, r = document) => r.querySelector(s);
const $$ = (s, r = document) => [...r.querySelectorAll(s)];
const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const fmtTime = s => { s = Math.max(0, Math.floor(s || 0)); return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`; };
const fmtBytes = b => { if (!b) return '0 B'; const u = ['B', 'KB', 'MB', 'GB', 'TB']; const i = Math.min(u.length - 1, Math.floor(Math.log(b) / Math.log(1024))); return `${(b / 1024 ** i).toFixed(i ? 1 : 0)} ${u[i]}`; };
const ago = ms => { if (!ms) return 'never'; const d = Date.now() - ms; if (d < 60e3) return 'just now'; if (d < 3600e3) return `${Math.floor(d / 60e3)} min ago`; if (d < 86400e3) return `${Math.floor(d / 3600e3)} h ago`; return new Date(ms).toLocaleDateString(); };

async function api(path, opts = {}) {
  const init = { method: opts.method || (opts.body !== undefined ? 'POST' : 'GET'), headers: {} };
  if (opts.body !== undefined) { init.headers['Content-Type'] = 'application/json'; init.body = JSON.stringify(opts.body); }
  const r = await fetch(path, init);
  const text = await r.text();
  let data = null; try { data = text ? JSON.parse(text) : null; } catch { data = text; }
  if (!r.ok) throw new Error((data && data.error) || `${r.status} ${r.statusText}`);
  return data;
}
function toast(msg, isErr) { const d = document.createElement('div'); d.textContent = msg; if (isErr) d.style.borderColor = 'var(--danger)'; $('#toast').appendChild(d); setTimeout(() => d.remove(), 6000); }
const guard = fn => (...a) => fn(...a).catch(e => toast(e.message, true));

/* ---------- tabs ---------- */
const loaders = {};
$$('nav button').forEach(b => b.addEventListener('click', () => showTab(b.dataset.tab)));
function showTab(name) {
  $$('nav button').forEach(b => b.classList.toggle('active', b.dataset.tab === name));
  $$('.tab').forEach(t => t.classList.toggle('active', t.id === `tab-${name}`));
  location.hash = name;
  loaders[name]?.();
}

/* ---------- status / settings ---------- */
let status = {};
async function loadStatus() {
  status = await api('/api/status');
  $('#desktop-name').textContent = status.name;
  $('#relay-url').textContent = status.relay;
  $('#data-dir').textContent = status.dataDir;
  $('#about').innerHTML = kv({ Version: status.version, 'Data directory': status.dataDir, 'LAN addresses': (status.lan || []).join(', ') || 'none found', 'Audio engine': status.audio?.enabled ? `on (${status.audio.version}${status.audio.headless ? ', headless' : ''})` : 'off', 'Library': `${status.library.tracks} tracks, ${fmtBytes(status.library.bytes)}`, 'Uptime': fmtTime(status.uptimeSec) });
}
function kv(obj) { return `<dl class="kv">${Object.entries(obj).map(([k, v]) => `<dt>${esc(k)}</dt><dd>${v}</dd>`).join('')}</dl>`; }

loaders.settings = guard(async () => {
  await loadStatus();
  const s = await api('/api/settings');
  const f = $('#settings-form');
  for (const el of f.elements) if (el.name && s[el.name] !== undefined) el.value = s[el.name];
});
$('#settings-form').addEventListener('submit', guard(async e => {
  e.preventDefault();
  const f = e.target; const body = {};
  for (const el of f.elements) if (el.name) body[el.name] = el.type === 'number' || el.name === 'crossfade_curve' ? Number(el.value) : el.value;
  await api('/api/settings', { method: 'PUT', body });
  toast('Saved'); loadStatus();
}));

/* ---------- link a phone ---------- */
$('#pair-new').addEventListener('click', guard(async () => {
  const r = await api('/api/pair/new', { body: {} });
  $('#pair-qr').innerHTML = r.svg;
  $('#pair-link').textContent = r.link;
  $('#pair-addrs').textContent = `Addresses in the code: ${r.addresses.join(', ') || 'none (is this machine on a network?)'}${r.funnel ? ` · funnel ${r.funnel}` : ''} · expires in 10 minutes`;
}));
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
      if (act === 'play') await api('/api/player/load', { body: { sha256: t.sha256 } });
      if (act === 'queue') { await api('/api/player/queue', { body: { sha256: t.sha256 } }); toast(`Queued ${t.title}`); }
      if (act === 'share') { const r = await api(`/api/share/${t.sha256}`, { body: {} }); showShare(r); }
    }));
    tb.appendChild(tr);
  }
});
function showShare(r) {
  const node = $('#share-dialog').content.cloneNode(true);
  node.querySelector('.qr').innerHTML = r.svg; node.querySelector('code').textContent = r.link;
  const modal = node.querySelector('.modal');
  node.querySelector('.close').addEventListener('click', () => modal.remove());
  modal.addEventListener('click', e => { if (e.target === modal) modal.remove(); });
  document.body.appendChild(node);
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

/* ---------- stations ---------- */
let hosting = false;
loaders.stations = guard(async () => {
  const [feed, link, host] = await Promise.all([api('/api/stations/feed'), api('/api/stations/link'), api('/api/stations/host')]);
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
    row.innerHTML = `<div class="grow"><div class="title">${esc(f?.name || st?.name || s?.npub?.slice(0, 16) || '')}${s?.mine ? ' <span class="badge">this desktop</span>' : ''} ${live}</div><div class="sub">${now}${st?.next?.length ? ` · next: ${esc(st.next[0].artist)} – ${esc(st.next[0].title)}` : ''}${s ? ` · via ${esc(s.via)}` : ''}</div></div>${f ? `<button class="danger">Unfollow</button>` : ''}`;
    if (f) row.querySelector('button').addEventListener('click', guard(async () => { await api(`/api/stations/follows/${f.pubkey}`, { method: 'DELETE' }); loaders.stations(); }));
    el.appendChild(row);
  }
});
$('#follow-form').addEventListener('submit', guard(async e => { e.preventDefault(); await api('/api/stations/follows', { body: { text: $('#follow-text').value } }); $('#follow-text').value = ''; loaders.stations(); }));
$('#host-toggle').addEventListener('click', guard(async () => { await api('/api/stations/host', { body: { on: !hosting, name: $('#station-name').value } }); loaders.stations(); }));
$('#reveal-nsec').addEventListener('click', guard(async () => { const r = await api('/api/identity?reveal=true'); $('#nsec').textContent = r.nsec; }));

/* ---------- relay & funnel ---------- */
loaders.relay = guard(async () => {
  const [r, s, f] = await Promise.all([api('/api/relay/stats'), api('/api/settings'), api('/api/funnel')]);
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

/* ---------- downloads ---------- */
loaders.downloads = guard(async () => {
  await loadStatus();
  const c = status.companion || {};
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

/* ---------- player bar + live events ---------- */
const canvas = $('#spectrum'); const ctx = canvas.getContext('2d');
let seeking = false;
function drawSpectrum(bands) {
  const w = canvas.width, h = canvas.height; ctx.clearRect(0, 0, w, h);
  if (!bands || !bands.length) return;
  const bw = w / bands.length;
  for (let i = 0; i < bands.length; i++) { const v = Math.max(0, Math.min(1, bands[i])); ctx.fillStyle = `hsl(${165 - v * 60} 80% ${45 + v * 20}%)`; ctx.fillRect(i * bw + 1, h - v * h, Math.max(1, bw - 2), v * h); }
}
function renderPlayer(st) {
  const cur = st.current;
  $('#now-title').textContent = cur ? cur.title : (st.engine ? 'Nothing playing' : 'Audio engine off');
  $('#now-artist').textContent = cur ? `${cur.artist}${cur.album ? ' · ' + cur.album : ''}${st.crossfading ? ' · crossfading' : ''}` : '';
  $('#p-play').textContent = st.playing ? '❙❙' : '▶';
  if (!seeking) $('#p-seek').value = st.durationSec ? Math.round(1000 * st.positionSec / st.durationSec) : 0;
  $('#p-time').textContent = `${fmtTime(st.positionSec)} / ${fmtTime(st.durationSec)}`;
  $('#p-queue').textContent = st.queue ? `${st.queue} queued` : '';
}
$('#p-play').addEventListener('click', guard(async () => { const st = await api('/api/player'); await api(st.playing ? '/api/player/pause' : '/api/player/play', { body: {} }); }));
$('#p-next').addEventListener('click', guard(async () => { await api('/api/player/next', { body: {} }); }));
$('#p-stop').addEventListener('click', guard(async () => { await api('/api/player/stop', { body: {} }); }));
$('#p-seek').addEventListener('input', () => { seeking = true; });
$('#p-seek').addEventListener('change', guard(async e => { seeking = false; const st = await api('/api/player'); await api('/api/player/seek', { body: { positionSec: st.durationSec * e.target.value / 1000 } }); }));
$('#p-vol').addEventListener('change', guard(async e => { await api('/api/player/volume', { body: { volume: e.target.value / 100 } }); }));

function connectEvents() {
  const ws = new WebSocket(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/ws/events`);
  ws.onopen = () => $('#live-dot').classList.add('on');
  ws.onclose = () => { $('#live-dot').classList.remove('on'); setTimeout(connectEvents, 2000); };
  ws.onmessage = ev => {
    const m = JSON.parse(ev.data);
    switch (m.type) {
      case 'player': renderPlayer(m.status); drawSpectrum(m.spectrum); break;
      case 'devicePaired': toast(`${m.name} linked`); if ($('#tab-link').classList.contains('active')) loaders.link(); $('#pair-qr').innerHTML = '<button id="pair-new" class="primary">Show a pairing code</button>'; $('#pair-qr button').addEventListener('click', () => $('#pair-new').click()); break;
      case 'trackAdded': toast(`Added ${m.artist} – ${m.title}`); if ($('#tab-library').classList.contains('active')) loaders.library(); if ($('#tab-backup').classList.contains('active')) loaders.backup(); break;
      case 'station': if ($('#tab-stations').classList.contains('active')) loaders.stations(); break;
      case 'relay': if ($('#tab-relay').classList.contains('active')) loaders.relay(); break;
      case 'companion': toast(`Download ready: ${m.message}`); if ($('#tab-downloads').classList.contains('active')) loaders.downloads(); break;
      case 'notice': toast(m.text); break;
    }
  };
}

/* ---------- boot ---------- */
guard(async () => { await loadStatus(); connectEvents(); showTab(location.hash.slice(1) || 'link'); })();
