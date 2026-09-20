/* ═══════════════════════════════════════════════════════════════════
   Nexus Panel – Frontend SPA
   Vanilla JS, no build step, no framework.
   ═══════════════════════════════════════════════════════════════════ */

const API = '/api/v1';

// ── Auth / session token ──────────────────────────────────────────

const TOKEN_KEY = 'nexus_token';

const Auth = {
  get token() { return localStorage.getItem(TOKEN_KEY) || ''; },
  set token(v) {
    if (v) localStorage.setItem(TOKEN_KEY, v);
    else localStorage.removeItem(TOKEN_KEY);
  },
  clear() { localStorage.removeItem(TOKEN_KEY); },
};

// ── Global state ──────────────────────────────────────────────────

const NX = window.NX = {
  containers: [],
  // 'admin', or 'servers' for a customer who arrived through their billing
  // portal and may only see the servers they pay for.
  scope: 'admin',
  serverIds: [],
  currentServer: null,
  currentPath: '/',
  term: null,
  refreshTimer: null,
};

// ── Blueprints (static catalog) ───────────────────────────────────

const BLUEPRINTS = [
  { id: 'minecraft-paper', name: 'Minecraft Paper', desc: 'Paper 1.21 with Aikar\'s JVM flags, auto-scaling, and plugin support.', tags: ['java','popular','auto-scale'], game: 'minecraft' },
  { id: 'rust',            name: 'Rust',            desc: 'Rust Dedicated Server with Oxide mod support and DDoS protection.', tags: ['popular','oxide','anticheat'], game: 'rust' },
  { id: 'valheim',         name: 'Valheim',         desc: 'Valheim with BepInEx mod framework and automatic updates.', tags: ['survival','mods','bepinex'], game: 'valheim' },
  { id: 'cs2',             name: 'Counter-Strike 2', desc: 'CS2 with GSLT, competitive configs, and workshop map support.', tags: ['competitive','esports','srcds'], game: 'cs2' },
  { id: 'palworld',        name: 'Palworld',        desc: 'Palworld Dedicated Server with optimised memory and CPU settings.', tags: ['popular','survival'], game: 'palworld' },
  { id: 'dayz',            name: 'DayZ',            desc: 'DayZ Dedicated Server with Steam Workshop mod support.', tags: ['survival','pvp','workshop'], game: 'dayz' },
];

// ── Toast notifications ───────────────────────────────────────────

function toast(msg, type = 'info') {
  const el = document.createElement('div');
  el.className = `toast toast-${type}`;
  el.textContent = msg;
  document.getElementById('toast-container').appendChild(el);
  setTimeout(() => el.remove(), 4000);
}

// ── API helpers ───────────────────────────────────────────────────

async function api(path, opts = {}) {
  const headers = { 'Content-Type': 'application/json', ...opts.headers };
  if (Auth.token) headers['Authorization'] = 'Bearer ' + Auth.token;

  const res = await fetch(API + path, { headers, ...opts });

  // Session expired or missing — drop the token and show the login screen.
  if (res.status === 401) {
    Auth.clear();
    showLogin('Your session expired. Please sign in again.');
    throw new Error('Authentication required');
  }

  if (!res.ok) {
    const body = await res.json().catch(() => ({ error: res.statusText }));
    throw new Error(body.error || res.statusText);
  }
  const ct = res.headers.get('content-type') || '';
  return ct.includes('json') ? res.json() : res.text();
}

// ── Router ────────────────────────────────────────────────────────

function route() {
  const hash = location.hash || '#/';
  const parts = hash.slice(2).split('/');
  let page = parts[0] || 'dashboard';

  // A customer's session has no node-level pages; anything but their
  // servers routes home rather than to a page full of 403s.
  if (NX.scope !== 'admin' && !['', 'dashboard', 'servers', 'marketplace'].includes(page)) {
    location.hash = '#/';
    return;
  }

  // Highlight active nav item
  document.querySelectorAll('.nav-item').forEach(el => {
    el.classList.toggle('active', el.dataset.page === page || (el.dataset.page === 'dashboard' && page === ''));
  });

  clearInterval(NX.refreshTimer);

  switch (page) {
    case '':
    case 'dashboard':   renderDashboard(); break;
    case 'servers':
      if (parts[1]) { renderServerDetail(parts[1]); }
      else { renderDashboard(); }
      break;
    case 'blueprints':  renderBlueprints(); break;
    case 'marketplace': renderMarketplace(); break;
    case 'analytics':   renderAnalytics(); break;
    case 'security':    renderPage('security'); break;
    case 'settings':    renderSettings(); break;
    case 'users':       renderPage('users'); break;
    default:            renderDashboard();
  }
}

function renderPage(tmplId) {
  const tmpl = document.getElementById('tmpl-' + tmplId);
  if (!tmpl) return;
  const content = document.getElementById('page-content');
  content.innerHTML = '';
  content.appendChild(tmpl.content.cloneNode(true));
}

// ── Dashboard ─────────────────────────────────────────────────────

async function renderDashboard() {
  renderPage('dashboard');

  try {
    // A customer's session cannot read node stats; their dashboard is just
    // their servers.
    if (NX.scope !== 'admin') {
      const containers = await api('/containers');
      NX.containers = containers;
      document.getElementById('stats-grid').innerHTML = '';
      renderServerTable(containers);
      NX.refreshTimer = setInterval(async () => {
        try {
          const c = await api('/containers');
          NX.containers = c;
          renderServerTable(c);
        } catch (_) {}
      }, 5000);
      return;
    }

    const [info, containers] = await Promise.all([
      api('/node/info'),
      api('/containers'),
    ]);

    NX.containers = containers;

    // Sidebar info
    document.getElementById('sidebar-node-id').textContent = info.node_id;
    document.getElementById('sidebar-version').textContent = 'v' + info.version;

    // Stats
    const mem_pct = info.system.total_memory_bytes > 0
      ? ((info.system.used_memory_bytes / info.system.total_memory_bytes) * 100).toFixed(1)
      : 0;
    const disk_pct = info.system.total_disk_bytes > 0
      ? ((info.system.used_disk_bytes / info.system.total_disk_bytes) * 100).toFixed(1)
      : 0;

    document.getElementById('stats-grid').innerHTML = `
      <div class="stat-card">
        <div class="stat-label">Total Servers</div>
        <div class="stat-value">${info.containers_total}</div>
      </div>
      <div class="stat-card">
        <div class="stat-label">Online</div>
        <div class="stat-value text-success">${info.containers_running}</div>
        <div class="stat-sub">${info.containers_total > 0 ? ((info.containers_running / info.containers_total) * 100).toFixed(0) : 0}% uptime</div>
      </div>
      <div class="stat-card">
        <div class="stat-label">Memory</div>
        <div class="stat-value">${mem_pct}%</div>
        <div class="stat-sub">${fmtBytes(info.system.used_memory_bytes)} / ${fmtBytes(info.system.total_memory_bytes)}</div>
      </div>
      <div class="stat-card">
        <div class="stat-label">Disk</div>
        <div class="stat-value">${disk_pct}%</div>
        <div class="stat-sub">${fmtBytes(info.system.used_disk_bytes)} / ${fmtBytes(info.system.total_disk_bytes)}</div>
      </div>
    `;

    renderServerTable(containers);

    // Auto-refresh every 5s
    NX.refreshTimer = setInterval(async () => {
      try {
        const c = await api('/containers');
        NX.containers = c;
        renderServerTable(c);
      } catch (_) {}
    }, 5000);

  } catch (err) {
    toast('Failed to load dashboard: ' + err.message, 'error');
  }
}

function renderServerTable(containers) {
  const tbody = document.getElementById('server-table-body');
  const empty = document.getElementById('no-servers');
  if (!tbody) return;

  if (containers.length === 0) {
    tbody.innerHTML = '';
    if (empty) empty.style.display = '';
    return;
  }

  if (empty) empty.style.display = 'none';

  tbody.innerHTML = containers.map(c => `
    <tr>
      <td>
        <div class="server-cell">
          <a href="#/servers/${c.id}" class="server-name">${esc(c.name)}</a>
          <span class="server-id">${c.id.slice(0, 12)}</span>
        </div>
      </td>
      <td>${statusBadge(c.status)}</td>
      <td class="text-muted text-sm">${esc(c.image)}</td>
      <td class="text-muted">${c.restart_count}</td>
      <td class="text-muted text-sm">${fmtTime(c.created_at)}</td>
      <td>
        <div class="btn-group">
          ${c.status === 'running'
            ? `<button class="btn btn-xs btn-danger" onclick="NX.stopServer('${c.id}')">Stop</button>
               <button class="btn btn-xs" onclick="NX.restartServer('${c.id}')">Restart</button>`
            : `<button class="btn btn-xs btn-success" onclick="NX.startServer('${c.id}')">Start</button>`
          }
          <a href="#/servers/${c.id}" class="btn btn-xs">Manage</a>
        </div>
      </td>
    </tr>
  `).join('');
}

NX.filterServers = function(q) {
  const filtered = NX.containers.filter(c =>
    c.name.toLowerCase().includes(q.toLowerCase()) ||
    c.id.toLowerCase().includes(q.toLowerCase())
  );
  renderServerTable(filtered);
};

// ── Server power actions ──────────────────────────────────────────

NX.startServer = async function(id) {
  try { await api(`/containers/${id}/start`, { method: 'POST' }); toast('Server starting', 'success'); route(); }
  catch (e) { toast(e.message, 'error'); }
};

NX.stopServer = async function(id) {
  try { await api(`/containers/${id}/stop`, { method: 'POST', body: '{}' }); toast('Server stopping', 'success'); route(); }
  catch (e) { toast(e.message, 'error'); }
};

NX.restartServer = async function(id) {
  try { await api(`/containers/${id}/restart`, { method: 'POST' }); toast('Server restarting', 'success'); route(); }
  catch (e) { toast(e.message, 'error'); }
};

NX.deleteServer = async function(id) {
  if (!confirm('Delete this server? This cannot be undone.')) return;
  try { await api(`/containers/${id}`, { method: 'DELETE' }); toast('Server deleted', 'success'); location.hash = '#/'; }
  catch (e) { toast(e.message, 'error'); }
};

// ── Server Detail ─────────────────────────────────────────────────

// Action buttons for a server. A server whose game files are not installed
// cannot start, so it is offered the install instead of a button that fails.
NX.renderDetailActions = function(id, c) {
  const el = document.getElementById('detail-actions');
  if (!el) return;
  const needsInstall = c.install_state === 'pending' || c.install_state === 'failed';
  const installing = c.install_state === 'running';
  // A customer's server exists because their billing system created it;
  // only that system's termination removes it, so they get no Delete.
  const del = NX.scope === 'admin'
    ? `<button class="btn btn-sm btn-danger" onclick="NX.deleteServer('${id}')">Delete</button>`
    : '';
  el.innerHTML = `
    ${c.status === 'running'
      ? `<button class="btn btn-sm btn-danger" onclick="NX.stopServer('${id}')">Stop</button>
         <button class="btn btn-sm" onclick="NX.restartServer('${id}')">Restart</button>
         ${del}`
      : c.status === 'suspended'
      ? `<span class="badge badge-warning">Suspended by billing</span> ${del}`
      : installing
      ? `<button class="btn btn-sm" onclick="NX.switchTab('install')">Installing…</button>
         ${del}`
      : needsInstall
      ? `<button class="btn btn-sm btn-primary" onclick="NX.switchTab('install')">Install game files</button>
         ${del}`
      : `<button class="btn btn-sm btn-success" onclick="NX.startServer('${id}')">Start</button>
         ${del}`
    }
  `;
};

async function renderServerDetail(id) {
  renderPage('server-detail');
  NX.currentServer = id;

  try {
    const c = await api(`/containers/${id}`);

    document.getElementById('detail-server-name').textContent = c.name;

    // Status bar
    document.getElementById('detail-status-bar').innerHTML = `
      <div class="status-item">
        <span class="status-item-label">Status</span>
        <span class="status-item-value">${statusBadge(c.status)}</span>
      </div>
      <div class="status-item">
        <span class="status-item-label">Game files</span>
        <span class="status-item-value">${installBadge(c.install_state)}</span>
      </div>
      <div class="status-item">
        <span class="status-item-label">Image</span>
        <span class="status-item-value text-sm">${esc(c.image)}</span>
      </div>
      <div class="status-item">
        <span class="status-item-label">PID</span>
        <span class="status-item-value">${c.pid || '—'}</span>
      </div>
      <div class="status-item">
        <span class="status-item-label">Restarts</span>
        <span class="status-item-value">${c.restart_count}${c.crash_count ? ` <span class="text-danger" title="Crashes detected">(${c.crash_count} crash${c.crash_count === 1 ? '' : 'es'})</span>` : ''}</span>
      </div>
      <div class="status-item">
        <span class="status-item-label">Disk</span>
        <span class="status-item-value${c.disk_limit_bytes && c.disk_used_bytes > c.disk_limit_bytes ? ' text-danger' : ''}">${fmtBytes(c.disk_used_bytes)}${c.disk_limit_bytes ? ' / ' + fmtBytes(c.disk_limit_bytes) : ''}</span>
      </div>
      <div class="status-item">
        <span class="status-item-label">Uptime</span>
        <span class="status-item-value">${c.started_at ? fmtDuration(Date.now()/1000 - c.started_at) : '—'}</span>
      </div>
    `;

    NX.renderDetailActions(id, c);
    const installing = c.install_state === 'running';

    // An install kicked off at creation time is already running when the
    // operator first opens the server; follow it without being asked.
    if (installing) NX.pollGameFiles();

    // Init console tab
    initConsole();

    // Server settings info
    document.getElementById('server-settings-info').innerHTML = `
      <div class="kv-row"><span class="kv-key">ID</span><span class="kv-value">${c.id}</span></div>
      <div class="kv-row"><span class="kv-key">Name</span><span class="kv-value">${esc(c.name)}</span></div>
      <div class="kv-row"><span class="kv-key">Image</span><span class="kv-value">${esc(c.image)}</span></div>
      <div class="kv-row"><span class="kv-key">Status</span><span class="kv-value">${c.status}</span></div>
      <div class="kv-row"><span class="kv-key">Created</span><span class="kv-value">${new Date(c.created_at * 1000).toLocaleString()}</span></div>
      <div class="kv-row"><span class="kv-key">Started</span><span class="kv-value">${c.started_at ? new Date(c.started_at * 1000).toLocaleString() : '—'}</span></div>
      <div class="kv-row"><span class="kv-key">Exit Code</span><span class="kv-value">${c.exit_code ?? '—'}</span></div>
    `;

    // Auto-refresh status every 5s
    // Re-render when anything the header shows has changed (status, PID,
    // crash count, disk), without disturbing the tab the operator is on.
    let last = JSON.stringify([c.status, c.pid, c.install_state, c.crash_count, c.disk_used_bytes, c.restart_count]);
    NX.refreshTimer = setInterval(async () => {
      try {
        const u = await api(`/containers/${id}`);
        const now = JSON.stringify([u.status, u.pid, u.install_state, u.crash_count, u.disk_used_bytes, u.restart_count]);
        if (now !== last) {
          last = now;
          const bar = document.getElementById('detail-status-bar');
          const cells = bar.querySelectorAll('.status-item-value');
          if (cells[0]) cells[0].innerHTML = statusBadge(u.status);
          if (cells[1]) cells[1].innerHTML = installBadge(u.install_state);
          if (cells[3]) cells[3].textContent = u.pid || '—';
          if (cells[4]) cells[4].innerHTML = `${u.restart_count}${u.crash_count ? ` <span class="text-danger">(${u.crash_count} crash${u.crash_count === 1 ? '' : 'es'})</span>` : ''}`;
          if (cells[5]) cells[5].textContent = `${fmtBytes(u.disk_used_bytes)}${u.disk_limit_bytes ? ' / ' + fmtBytes(u.disk_limit_bytes) : ''}`;
          NX.renderDetailActions(id, u);
        }
        const uptime = document.querySelector('#detail-status-bar .status-item:last-child .status-item-value');
        if (uptime) uptime.textContent = u.started_at && u.status === 'running' ? fmtDuration(Date.now()/1000 - u.started_at) : '—';
      } catch (_) {}
    }, 5000);

  } catch (err) {
    toast('Failed to load server: ' + err.message, 'error');
  }
}

// ── Tabs ──────────────────────────────────────────────────────────

NX.switchTab = function(tab) {
  document.querySelectorAll('.tab').forEach(el => el.classList.toggle('active', el.dataset.tab === tab));
  document.querySelectorAll('.tab-content').forEach(el => el.classList.toggle('hidden', el.id !== 'tab-' + tab));

  if (tab === 'files') loadFiles('/');
  if (tab === 'backups') loadBackups();
  if (tab === 'schedules') loadSchedules();
  if (tab === 'shell') { const i = document.getElementById('shell-input'); if (i) i.focus(); }
  if (tab === 'update') { NX.loadUpdateConfig(); NX.refreshUpdateStatus(); }
  if (tab === 'install') NX.refreshGameFiles();
};

// ── Install game files ────────────────────────────────────────────

// How a server's install state reads in the UI.
const INSTALL_STATE_LABEL = {
  pending: ['badge-warning', 'Not installed'],
  running: ['badge-warning', 'Installing…'],
  installed: ['badge-success', 'Installed'],
  failed: ['badge-danger', 'Install failed'],
  not_required: ['badge-success', 'No install needed'],
  unknown: ['badge-secondary', 'Unknown'],
};

function installBadge(state) {
  const [cls, label] = INSTALL_STATE_LABEL[state] || ['badge-secondary', state || 'unknown'];
  return `<span class="badge ${cls}">${esc(label)}</span>`;
}

// Show where this server stands and whether installing is possible now.
NX.refreshGameFiles = async function() {
  if (!NX.currentServer) return;
  const summary = document.getElementById('gamefiles-summary');
  const btn = document.getElementById('gamefiles-run');

  try {
    const c = await api(`/containers/${NX.currentServer}`);
    NX.gameFilesState = c.install_state;
    if (summary) {
      summary.innerHTML = `
        <div class="text-sm"><strong>Status</strong> ${installBadge(c.install_state)}</div>
        <div class="text-muted text-sm" style="margin-top:0.35rem">
          ${c.install_state === 'not_required'
            ? 'This game needs no install step — its image is self-contained.'
            : 'Installing downloads this game into the server folder. The server must be stopped.'}
        </div>`;
    }
    if (btn) {
      btn.disabled = c.status === 'running' || c.install_state === 'not_required';
      btn.textContent = c.install_state === 'installed' ? 'Reinstall game files' : 'Install game files';
    }
  } catch (_) { /* server list will report this */ }

  try {
    const job = await api(`/containers/${NX.currentServer}/install`);
    NX.renderGameFilesJob(job);
    if (job.status === 'running') NX.pollGameFiles();
  } catch (_) {
    // Nothing installed on this node yet — the button is the next step.
  }
};

NX.startGameFilesInstall = async function() {
  if (!NX.currentServer) return;
  const status = document.getElementById('gamefiles-status');
  const btn = document.getElementById('gamefiles-run');
  status.innerHTML = '<span class="text-muted">Starting install…</span>';
  if (btn) btn.disabled = true;
  try {
    await api(`/containers/${NX.currentServer}/install`, { method: 'POST' });
    toast('Install started', 'success');
    NX.pollGameFiles();
  } catch (e) {
    status.innerHTML = `<span class="text-danger">${esc(e.message)}</span>`;
    if (btn) btn.disabled = false;
  }
};

NX.renderGameFilesJob = function(job) {
  const status = document.getElementById('gamefiles-status');
  const out = document.getElementById('gamefiles-output');
  const btn = document.getElementById('gamefiles-run');
  if (!status) return;
  const badge = {
    running: '<span class="badge badge-warning">Installing…</span>',
    succeeded: '<span class="badge badge-success">Installed</span>',
    failed: '<span class="badge badge-danger">Failed</span>',
  }[job.status] || esc(job.status);
  let line = `${badge} <span class="text-muted text-sm">in ${esc(job.image)}</span>`;
  if (job.exit_code !== null && job.exit_code !== undefined) {
    line += ` <span class="text-muted text-sm">(exit ${job.exit_code})</span>`;
  }
  status.innerHTML = line;
  const text = [job.log, job.error ? `error: ${job.error}` : ''].filter(Boolean).join('\n');
  if (text && out) {
    out.textContent = text;
    out.classList.remove('hidden');
    out.scrollTop = out.scrollHeight;
  }
  if (btn) btn.disabled = job.status === 'running';
};

NX.pollGameFiles = function() {
  clearTimeout(NX.gameFilesPollTimer);
  NX.gameFilesPollTimer = setTimeout(async () => {
    if (!NX.currentServer) return;
    try {
      const job = await api(`/containers/${NX.currentServer}/install`);
      NX.renderGameFilesJob(job);
      if (job.status === 'running') {
        NX.pollGameFiles();
      } else {
        // The server's own state changed with it: Start may now be allowed.
        NX.refreshGameFiles();
        renderServerDetail(NX.currentServer);
      }
    } catch (_) {}
  }, 2000);
};

// ── Update game files ─────────────────────────────────────────────

// Human-readable one-liner for a blueprint's `updates.apply` strategy.
function updateApplySummary(a) {
  if (!a || !a.type) return 'unknown strategy';
  switch (a.type) {
    case 'steam_cmd':
      return `SteamCMD · app ${a.app_id}` + (a.beta ? ` · beta ${a.beta}` : '');
    case 'depot_downloader':
      return `DepotDownloader · app ${a.app_id}`
        + (a.depot_id ? ` · depot ${a.depot_id}` : '')
        + (a.branch ? ` · branch ${a.branch}` : '');
    case 'download': return `Download · ${a.url}`;
    case 'command': return `Command · ${a.command}`;
    case 'docker': return 'Docker image pull (runs at the host level, not in-container)';
    default: return a.type;
  }
}

// Load the update strategy this server's blueprint declares, so the operator
// can run it in one click instead of retyping it.
NX.loadUpdateConfig = async function() {
  const box = document.getElementById('update-blueprint');
  const manual = document.getElementById('update-manual');
  if (!box || !NX.currentServer) return;
  try {
    const cfg = await api(`/containers/${NX.currentServer}/update-config`);
    NX.updateBlueprint = cfg;
    const runnable = cfg.apply && cfg.apply.type !== 'docker';
    box.innerHTML = `
      <div class="text-sm" style="margin-bottom:0.6rem">
        <strong>From this server's blueprint</strong>
        <div class="text-muted" style="margin-top:0.25rem">${esc(updateApplySummary(cfg.apply))}</div>
        <div class="text-muted" style="margin-top:0.15rem">Installs into <code>${esc(cfg.install_dir)}</code></div>
      </div>
      ${runnable
        ? `<button class="btn btn-primary btn-sm" id="update-run-blueprint" onclick="NX.startUpdate(true)">Run blueprint update</button>`
        : `<span class="text-muted text-sm">This strategy can't be applied from inside the container.</span>`}`;
    box.classList.remove('hidden');
    if (manual) manual.open = false;
    NX.prefillUpdateForm(cfg);
  } catch (_) {
    // No blueprint on file, or it declares no update strategy — manual only.
    NX.updateBlueprint = null;
    box.classList.add('hidden');
    if (manual) manual.open = true;
  }
};

// Mirror the blueprint's strategy into the manual override form.
NX.prefillUpdateForm = function(cfg) {
  const a = cfg && cfg.apply;
  if (!a) return;
  const dir = document.getElementById('update-dir');
  if (dir && cfg.install_dir) dir.value = cfg.install_dir;
  if (a.type !== 'steam_cmd' && a.type !== 'depot_downloader') return;
  const method = document.getElementById('update-method');
  const appId = document.getElementById('update-appid');
  const branch = document.getElementById('update-branch');
  const depot = document.getElementById('update-depot');
  if (method) method.value = a.type;
  if (appId && a.app_id != null) appId.value = a.app_id;
  if (branch) branch.value = a.beta || a.branch || '';
  if (depot) depot.value = a.depot_id != null ? a.depot_id : '';
  NX.onUpdateMethodChange();
};

NX.onUpdateMethodChange = function() {
  const method = document.getElementById('update-method')?.value;
  // DepotDownloader exposes a depot id; SteamCMD does not.
  const depotGroup = document.getElementById('update-depot-group');
  if (depotGroup) depotGroup.style.display = method === 'depot_downloader' ? '' : 'none';
};

NX.startUpdate = async function(useBlueprint) {
  if (!NX.currentServer) return;
  const status = document.getElementById('update-status');
  const btns = ['update-run', 'update-run-blueprint']
    .map(id => document.getElementById(id))
    .filter(Boolean);

  // One-click path: send an empty body and let the node resolve the strategy
  // from the server's stored blueprint.
  if (useBlueprint) {
    status.innerHTML = '<span class="text-muted">Starting update…</span>';
    btns.forEach(b => { b.disabled = true; });
    try {
      await api(`/containers/${NX.currentServer}/update`, { method: 'POST', body: '{}' });
      toast('Update started', 'success');
      NX.pollUpdate();
    } catch (e) {
      status.innerHTML = `<span class="text-danger">${esc(e.message)}</span>`;
      btns.forEach(b => { b.disabled = false; });
    }
    return;
  }

  const method = document.getElementById('update-method').value;
  const appId = parseInt(document.getElementById('update-appid').value.trim(), 10);
  const branch = document.getElementById('update-branch')?.value.trim();
  const depot = document.getElementById('update-depot')?.value.trim();
  const dir = document.getElementById('update-dir').value.trim();
  if (!Number.isFinite(appId)) {
    status.innerHTML = '<span class="text-danger">Enter a valid Steam App ID.</span>';
    return;
  }
  const body = { type: method, app_id: appId };
  if (dir) body.install_dir = dir;
  if (method === 'steam_cmd' && branch) body.beta = branch;
  if (method === 'depot_downloader') {
    if (branch) body.branch = branch;
    if (depot) body.depot_id = parseInt(depot, 10);
  }
  status.innerHTML = '<span class="text-muted">Starting update…</span>';
  btns.forEach(b => { b.disabled = true; });
  try {
    await api(`/containers/${NX.currentServer}/update`, {
      method: 'POST',
      body: JSON.stringify(body),
    });
    toast('Update started', 'success');
    NX.pollUpdate();
  } catch (e) {
    status.innerHTML = `<span class="text-danger">${esc(e.message)}</span>`;
    btns.forEach(b => { b.disabled = false; });
  }
};

NX.refreshUpdateStatus = async function() {
  if (!NX.currentServer) return;
  try {
    const job = await api(`/containers/${NX.currentServer}/update`);
    NX.renderUpdateJob(job);
    if (job.status === 'running') NX.pollUpdate();
  } catch (_) {
    // No update run yet — leave the form ready.
  }
};

NX.renderUpdateJob = function(job) {
  const status = document.getElementById('update-status');
  const out = document.getElementById('update-output');
  const btn = document.getElementById('update-run');
  if (!status) return;
  const badge = {
    running: '<span class="badge badge-warning">Running…</span>',
    succeeded: '<span class="badge badge-success">Succeeded</span>',
    failed: '<span class="badge badge-danger">Failed</span>',
  }[job.status] || esc(job.status);
  let line = `${badge} <code class="text-sm">${esc(job.command)}</code>`;
  if (job.exit_code !== null && job.exit_code !== undefined) line += ` <span class="text-muted text-sm">(exit ${job.exit_code})</span>`;
  status.innerHTML = line;
  const text = [job.stdout, job.stderr, job.error ? `error: ${job.error}` : ''].filter(Boolean).join('\n');
  if (text) { out.textContent = text; out.classList.remove('hidden'); out.scrollTop = out.scrollHeight; }
  const running = job.status === 'running';
  if (btn) btn.disabled = running;
  const bpBtn = document.getElementById('update-run-blueprint');
  if (bpBtn) bpBtn.disabled = running;
};

NX.pollUpdate = function() {
  clearTimeout(NX.updatePollTimer);
  NX.updatePollTimer = setTimeout(async () => {
    if (!NX.currentServer) return;
    try {
      const job = await api(`/containers/${NX.currentServer}/update`);
      NX.renderUpdateJob(job);
      if (job.status === 'running') NX.pollUpdate();
    } catch (_) {}
  }, 2000);
};

// ── Shell (container exec) ────────────────────────────────────────

NX.runShell = async function() {
  const input = document.getElementById('shell-input');
  const out = document.getElementById('shell-output');
  const btn = document.getElementById('shell-run');
  const cmd = input.value.trim();
  if (!cmd || !NX.currentServer) return;
  input.value = '';

  out.textContent += `$ ${cmd}\n`;
  btn.disabled = true;
  try {
    const res = await api(`/containers/${NX.currentServer}/exec`, {
      method: 'POST',
      body: JSON.stringify({ command: cmd }),
    });
    if (res.stdout) out.textContent += res.stdout;
    if (res.stderr) out.textContent += res.stderr;
    if (res.exit_code !== null && res.exit_code !== 0) {
      out.textContent += `[exit code ${res.exit_code}]\n`;
    }
  } catch (e) {
    out.textContent += `error: ${e.message}\n`;
  } finally {
    btn.disabled = false;
    out.scrollTop = out.scrollHeight;
    input.focus();
  }
};

// ── Console ───────────────────────────────────────────────────────

function initConsole() {
  const el = document.getElementById('terminal');
  if (!el || typeof Terminal === 'undefined') return;

  if (NX.term) { NX.term.dispose(); NX.term = null; }

  NX.term = new Terminal({
    theme: {
      background: '#000000',
      foreground: '#f8fafc',
      cursor: '#6366f1',
      cursorAccent: '#000000',
      selectionBackground: 'rgba(99,102,241,0.3)',
    },
    fontFamily: '"Fira Code", "Cascadia Code", "JetBrains Mono", monospace',
    fontSize: 13,
    cursorBlink: true,
    disableStdin: true,
  });
  NX.term.open(el);
  NX.term.writeln('\x1b[34m[nexus]\x1b[0m Console connected. Type commands below.');
}

NX.sendCommand = async function() {
  const input = document.getElementById('console-input');
  const cmd = input.value.trim();
  if (!cmd || !NX.currentServer) return;
  input.value = '';

  if (NX.term) NX.term.writeln(`\x1b[32m$ ${cmd}\x1b[0m`);

  try {
    await api(`/containers/${NX.currentServer}/command`, {
      method: 'POST',
      body: JSON.stringify({ command: cmd }),
    });
    if (NX.term) NX.term.writeln('\x1b[90mCommand sent.\x1b[0m');
  } catch (e) {
    if (NX.term) NX.term.writeln(`\x1b[31mError: ${e.message}\x1b[0m`);
  }
};

// ── Files ─────────────────────────────────────────────────────────

async function loadFiles(path) {
  NX.currentPath = path;
  const id = NX.currentServer;
  if (!id) return;

  try {
    const files = await api(`/containers/${id}/files?path=${encodeURIComponent(path)}`);

    // Breadcrumb
    const bc = document.getElementById('file-breadcrumb');
    if (bc) {
      const parts = path.split('/').filter(Boolean);
      let html = `<a class="file-name-link" onclick="loadFiles('/')">root</a>`;
      let acc = '';
      for (const p of parts) {
        acc += '/' + p;
        const a = acc;
        html += `<span class="separator">/</span><a class="file-name-link" onclick="loadFiles('${esc(a)}')">${esc(p)}</a>`;
      }
      bc.innerHTML = html;
    }

    // Table
    const tbody = document.getElementById('file-table-body');
    if (!tbody) return;

    // Sort: dirs first, then alpha
    files.sort((a, b) => {
      if (a.is_directory !== b.is_directory) return a.is_directory ? -1 : 1;
      return a.name.localeCompare(b.name);
    });

    tbody.innerHTML = files.map(f => {
      const icon = f.is_directory
        ? '<span class="file-icon dir">&#x1F4C1;</span>'
        : '<span class="file-icon">&#x1F4C4;</span>';
      const clickPath = f.path;
      const onclick = f.is_directory
        ? `onclick="loadFiles('${esc(clickPath)}')" style="cursor:pointer"`
        : `onclick="NX.editFile('${esc(clickPath)}')" style="cursor:pointer"`;

      return `<tr>
        <td>${icon}<span class="file-name-link" ${onclick}>${esc(f.name)}</span></td>
        <td class="text-muted text-sm">${f.is_directory ? '—' : fmtBytes(f.size)}</td>
        <td class="text-muted text-sm">${new Date(f.modified_at * 1000).toLocaleString()}</td>
        <td>
          <div class="btn-group">
            ${!f.is_directory ? `<button class="btn btn-xs" onclick="NX.editFile('${esc(clickPath)}')">Edit</button>` : ''}
            <button class="btn btn-xs btn-danger" onclick="NX.deleteFile('${esc(clickPath)}', ${f.is_directory})">Delete</button>
          </div>
        </td>
      </tr>`;
    }).join('');

  } catch (e) { toast('Failed to list files: ' + e.message, 'error'); }
}
// Expose to global for inline onclick
window.loadFiles = loadFiles;

NX.editFile = async function(path) {
  try {
    // Through api(), which carries the session; a bare fetch sent no
    // credentials and 401'd for every password-authenticated admin.
    const text = await api(`/containers/${NX.currentServer}/files/read?path=${encodeURIComponent(path)}`);

    document.getElementById('modal-title').textContent = 'Edit: ' + path.split('/').pop();
    document.getElementById('modal-body').innerHTML = `
      <textarea class="form-input" id="file-editor" style="width:100%;height:350px;font-size:13px">${esc(text)}</textarea>
      <div style="margin-top:1rem;text-align:right">
        <button class="btn btn-primary" onclick="NX.saveFile('${esc(path)}')">Save</button>
      </div>
    `;
    document.getElementById('modal-overlay').classList.remove('hidden');
  } catch (e) { toast(e.message, 'error'); }
};

NX.saveFile = async function(path) {
  const content = document.getElementById('file-editor').value;
  try {
    await api(`/containers/${NX.currentServer}/files/write`, {
      method: 'POST', body: JSON.stringify({ path, content })
    });
    toast('File saved', 'success');
    NX.closeModal();
    loadFiles(NX.currentPath);
  } catch (e) { toast(e.message, 'error'); }
};

NX.deleteFile = async function(path, isDir) {
  if (!confirm(`Delete ${isDir ? 'folder' : 'file'} "${path.split('/').pop()}"?`)) return;
  try {
    await api(`/containers/${NX.currentServer}/files/delete`, {
      method: 'POST', body: JSON.stringify({ paths: [path], recursive: isDir })
    });
    toast('Deleted', 'success');
    loadFiles(NX.currentPath);
  } catch (e) { toast(e.message, 'error'); }
};

NX.createNewFile = function() {
  const name = prompt('File name:');
  if (!name) return;
  const path = (NX.currentPath === '/' ? '/' : NX.currentPath + '/') + name;
  api(`/containers/${NX.currentServer}/files/write`, {
    method: 'POST', body: JSON.stringify({ path, content: '' })
  }).then(() => { toast('File created', 'success'); loadFiles(NX.currentPath); })
    .catch(e => toast(e.message, 'error'));
};

NX.createNewFolder = function() {
  const name = prompt('Folder name:');
  if (!name) return;
  const path = (NX.currentPath === '/' ? '/' : NX.currentPath + '/') + name;
  api(`/containers/${NX.currentServer}/files/mkdir`, {
    method: 'POST', body: JSON.stringify({ path, recursive: true })
  }).then(() => { toast('Folder created', 'success'); loadFiles(NX.currentPath); })
    .catch(e => toast(e.message, 'error'));
};

// ── Backups ───────────────────────────────────────────────────────

async function loadBackups() {
  const id = NX.currentServer;
  if (!id) return;
  try {
    const backups = await api(`/containers/${id}/backups`);
    const tbody = document.getElementById('backup-table-body');
    if (!tbody) return;

    if (backups.length === 0) {
      tbody.innerHTML = '<tr><td colspan="5" class="text-muted" style="text-align:center;padding:2rem">No backups yet</td></tr>';
      return;
    }

    tbody.innerHTML = backups.map(b => `
      <tr>
        <td>${esc(b.name)}</td>
        <td class="text-muted">${fmtBytes(b.size)}</td>
        <td class="text-muted text-sm">${new Date(b.created_at * 1000).toLocaleString()}</td>
        <td>${statusBadge(b.status)}</td>
        <td>
          <div class="btn-group">
            <button class="btn btn-xs btn-success" onclick="NX.restoreBackup('${b.id}')">Restore</button>
            <button class="btn btn-xs btn-danger" onclick="NX.deleteBackup('${b.id}')">Delete</button>
          </div>
        </td>
      </tr>
    `).join('');
  } catch (e) { toast(e.message, 'error'); }
}

NX.createBackup = async function() {
  const name = prompt('Backup name:', 'manual-backup');
  if (!name) return;
  try {
    await api(`/containers/${NX.currentServer}/backups`, {
      method: 'POST', body: JSON.stringify({ name })
    });
    toast('Backup created', 'success');
    loadBackups();
  } catch (e) { toast(e.message, 'error'); }
};

NX.restoreBackup = async function(backupId) {
  if (!confirm('Restore this backup? Current server files may be overwritten.')) return;
  try {
    await api(`/containers/${NX.currentServer}/backups/${backupId}/restore`, { method: 'POST' });
    toast('Backup restored', 'success');
  } catch (e) { toast(e.message, 'error'); }
};

NX.deleteBackup = async function(backupId) {
  if (!confirm('Delete this backup?')) return;
  try {
    await api(`/containers/${NX.currentServer}/backups/${backupId}`, { method: 'DELETE' });
    toast('Backup deleted', 'success');
    loadBackups();
  } catch (e) { toast(e.message, 'error'); }
};

// ── Schedules ─────────────────────────────────────────────────────

async function loadSchedules() {
  const id = NX.currentServer;
  if (!id) return;
  try {
    const schedules = await api(`/containers/${id}/schedules`);
    const tbody = document.getElementById('schedule-table-body');
    if (!tbody) return;

    if (schedules.length === 0) {
      tbody.innerHTML = '<tr><td colspan="6" class="text-muted" style="text-align:center;padding:2rem">No schedules yet</td></tr>';
      return;
    }

    tbody.innerHTML = schedules.map(s => `
      <tr>
        <td>${esc(s.name)}</td>
        <td class="text-muted text-sm" style="font-family:monospace">${esc(s.cron_expression)}</td>
        <td>${s.is_active
          ? '<span class="badge badge-success"><span class="badge-dot"></span>Active</span>'
          : '<span class="badge badge-muted"><span class="badge-dot"></span>Inactive</span>'}</td>
        <td class="text-muted text-sm">${s.last_run ? new Date(s.last_run * 1000).toLocaleString() : '—'}</td>
        <td class="text-muted text-sm">${s.next_run ? new Date(s.next_run * 1000).toLocaleString() : '—'}</td>
        <td>
          <div class="btn-group">
            <button class="btn btn-xs" onclick="NX.triggerSchedule('${s.id}')">Run Now</button>
            <button class="btn btn-xs btn-danger" onclick="NX.removeSchedule('${s.id}')">Delete</button>
          </div>
        </td>
      </tr>
    `).join('');
  } catch (e) { toast(e.message, 'error'); }
}

NX.showCreateSchedule = function() {
  document.getElementById('modal-title').textContent = 'Create Schedule';
  document.getElementById('modal-body').innerHTML = `
    <div class="form-group">
      <label class="form-label">Name</label>
      <input type="text" class="form-input" id="sched-name" style="width:100%" placeholder="Daily restart">
    </div>
    <div class="form-group">
      <label class="form-label">Cron Expression</label>
      <input type="text" class="form-input" id="sched-cron" style="width:100%" placeholder="0 4 * * *">
      <div class="text-muted text-xs mt-1">minute hour day month weekday</div>
    </div>
    <div class="form-group">
      <label class="form-label">Action</label>
      <select class="form-input" id="sched-action" style="width:100%">
        <option value="power">Power Action</option>
        <option value="command">Console Command</option>
        <option value="backup">Create Backup</option>
      </select>
    </div>
    <div class="form-group">
      <label class="form-label">Payload</label>
      <input type="text" class="form-input" id="sched-payload" style="width:100%" placeholder="restart / say Hello / backup-name">
    </div>
    <div style="text-align:right;margin-top:1rem">
      <button class="btn btn-primary" onclick="NX.createSchedule()">Create</button>
    </div>
  `;
  document.getElementById('modal-overlay').classList.remove('hidden');
};

NX.createSchedule = async function() {
  const name = document.getElementById('sched-name').value;
  const cron = document.getElementById('sched-cron').value;
  const action = document.getElementById('sched-action').value;
  const payload = document.getElementById('sched-payload').value;
  if (!name || !cron) return toast('Name and cron are required', 'error');
  try {
    await api(`/containers/${NX.currentServer}/schedules`, {
      method: 'POST',
      body: JSON.stringify({ name, cron_expression: cron, tasks: [{ action, payload, time_offset: 0 }] })
    });
    toast('Schedule created', 'success');
    NX.closeModal();
    loadSchedules();
  } catch (e) { toast(e.message, 'error'); }
};

NX.triggerSchedule = async function(scheduleId) {
  try {
    await api(`/containers/${NX.currentServer}/schedules/${scheduleId}/trigger`, { method: 'POST' });
    toast('Schedule triggered', 'success');
  } catch (e) { toast(e.message, 'error'); }
};

NX.removeSchedule = async function(scheduleId) {
  if (!confirm('Delete this schedule?')) return;
  try {
    await api(`/containers/${NX.currentServer}/schedules/${scheduleId}`, { method: 'DELETE' });
    toast('Schedule deleted', 'success');
    loadSchedules();
  } catch (e) { toast(e.message, 'error'); }
};

// ── Create Server ─────────────────────────────────────────────────

NX.showCreateServer = function(prefillYaml) {
  const yaml = prefillYaml || `metadata:
  name: My Server
  game: minecraft
  version: "1.0"
  author: admin

container:
  image: itzg/minecraft-server:latest

resources:
  cpu:
    min: 1000
    max: 2000
  memory:
    min: 1Gi
    max: 2Gi

startup:
  command: java
  args: ["-jar", "server.jar"]
  working_dir: /home/container

networking:
  ports:
    - name: game
      internal: "25565"
      protocol: tcp`;
  document.getElementById('modal-title').textContent = 'Create Server';
  document.getElementById('modal-body').innerHTML = `
    <div class="form-group">
      <label class="form-label">Blueprint Config (YAML)</label>
      <textarea class="form-input" id="create-yaml" style="width:100%;height:280px" placeholder="Paste your blueprint YAML here...">${esc(yaml)}</textarea>
    </div>
    <div class="form-group">
      <label class="form-label">
        <input type="checkbox" id="create-autostart" checked> Auto-start after creation
        <span class="text-muted text-sm">(games that need installing start once their files are in place)</span>
      </label>
    </div>
    <div style="text-align:right">
      <button class="btn btn-primary" onclick="NX.doCreateServer()">Create</button>
    </div>
  `;
  document.getElementById('modal-overlay').classList.remove('hidden');
};

NX.doCreateServer = async function() {
  const yaml = document.getElementById('create-yaml').value;
  const autoStart = document.getElementById('create-autostart').checked;
  if (!yaml.trim()) return toast('YAML config is required', 'error');
  try {
    const res = await api('/containers', {
      method: 'POST',
      body: JSON.stringify({ config_yaml: yaml, auto_start: autoStart })
    });
    toast(res.installing
      ? 'Server created — installing game files'
      : 'Server created: ' + res.id, 'success');
    NX.closeModal();
    location.hash = '#/servers/' + res.id;
    // The install starts with the server; open on it so the download is
    // visible rather than looking like a server that will not start.
    if (res.installing) setTimeout(() => NX.switchTab('install'), 100);
  } catch (e) { toast(e.message, 'error'); }
};

// ── Blueprints ────────────────────────────────────────────────────

function renderBlueprints() {
  renderPage('blueprints');
  const grid = document.getElementById('blueprint-grid');
  if (!grid) return;

  grid.innerHTML = BLUEPRINTS.map(bp => `
    <div class="blueprint-card" onclick="NX.deployBlueprint('${bp.id}')">
      <h3>${esc(bp.name)}</h3>
      <p>${esc(bp.desc)}</p>
      <div class="blueprint-tags">
        ${bp.tags.map(t => `<span class="blueprint-tag">${t}</span>`).join('')}
      </div>
    </div>
  `).join('');
}

// ── Pterodactyl egg import ────────────────────────────────────────

NX.importEgg = async function() {
  const src = document.getElementById('egg-json');
  const out = document.getElementById('egg-result');
  const btn = document.getElementById('egg-convert');
  const scan = document.getElementById('egg-scan');
  if (!src || !out) return;
  const eggJson = src.value.trim();
  if (!eggJson) {
    out.innerHTML = '<span class="text-danger text-sm">Paste an exported egg first.</span>';
    return;
  }

  out.innerHTML = '<span class="text-muted text-sm">Converting…</span>';
  btn.disabled = true;
  try {
    const res = await api('/blueprints/import-egg', {
      method: 'POST',
      body: JSON.stringify({ egg_json: eggJson, security_scan: !scan || scan.checked }),
    });
    NX.importedBlueprint = res.blueprint_yaml;

    const warnings = (res.warnings || []).length
      ? `<div style="margin-top:0.9rem">
           <div class="text-warning text-sm" style="font-weight:600;margin-bottom:0.35rem">
             ${res.warnings.length} security finding${res.warnings.length === 1 ? '' : 's'} in this egg
           </div>
           <ul class="text-sm text-muted" style="margin:0;padding-left:1.1rem">
             ${res.warnings.map(w => `<li>${esc(w)}</li>`).join('')}
           </ul>
           <p class="text-sm text-muted" style="margin-top:0.4rem">
             Eggs are third-party scripts — review these before deploying.
           </p>
         </div>`
      : '<div class="text-success text-sm" style="margin-top:0.9rem">No risky patterns found in this egg\'s scripts.</div>';

    out.innerHTML = `
      <div class="badge badge-success" style="margin-bottom:0.75rem">Converted</div>
      <div class="kv-row"><span class="kv-key">Name</span><span class="kv-value">${esc(res.name)}</span></div>
      <div class="kv-row"><span class="kv-key">Game</span><span class="kv-value">${esc(res.game)}</span></div>
      <div class="kv-row"><span class="kv-key">Image</span><span class="kv-value text-sm">${esc(res.image)}</span></div>
      <div class="kv-row"><span class="kv-key">Variables</span><span class="kv-value">${res.variable_count}</span></div>
      <div class="kv-row"><span class="kv-key">Ports</span><span class="kv-value">${res.port_count}</span></div>
      ${warnings}
      <details style="margin-top:0.9rem">
        <summary class="text-sm text-muted" style="cursor:pointer">View generated blueprint</summary>
        <pre class="terminal-box shell-output" style="margin-top:0.5rem;max-height:320px">${esc(res.blueprint_yaml)}</pre>
      </details>
      <div style="margin-top:1rem">
        <button class="btn btn-primary btn-sm" onclick="NX.useImportedBlueprint()">Create server from this</button>
      </div>`;
  } catch (e) {
    out.innerHTML = `<span class="text-danger text-sm">${esc(e.message)}</span>`;
  } finally {
    btn.disabled = false;
  }
};

NX.useImportedBlueprint = function() {
  if (!NX.importedBlueprint) return;
  NX.showCreateServer(NX.importedBlueprint);
};

NX.deployBlueprint = async function(bpId) {
  const bp = BLUEPRINTS.find(b => b.id === bpId);
  const label = bp ? bp.name : bpId;
  // The node serves the blueprints the repo ships, so the form is prefilled
  // with YAML that is known to parse rather than a copy kept here by hand.
  try {
    const yaml = await api(`/blueprints/${encodeURIComponent(bpId)}`);
    toast(`Loaded ${label} blueprint`, 'success');
    NX.showCreateServer(yaml);
  } catch (e) {
    toast(`Could not load ${label} blueprint: ${e.message}`, 'error');
  }
};

// ── Analytics ─────────────────────────────────────────────────────

async function renderAnalytics() {
  renderPage('analytics');
  try {
    const [info, health] = await Promise.all([api('/node/info'), api('/node/health')]);

    const el = document.getElementById('analytics-stats');
    if (el) {
      el.innerHTML = `
        <div class="stat-card"><div class="stat-label">Uptime</div><div class="stat-value">${fmtDuration(info.uptime_secs)}</div></div>
        <div class="stat-card"><div class="stat-label">CPUs</div><div class="stat-value">${info.system.cpu_count}</div></div>
        <div class="stat-card"><div class="stat-label">Total Memory</div><div class="stat-value">${fmtBytes(info.system.total_memory_bytes)}</div></div>
        <div class="stat-card"><div class="stat-label">Total Disk</div><div class="stat-value">${fmtBytes(info.system.total_disk_bytes)}</div></div>
      `;
    }

    const hc = document.getElementById('health-checks');
    if (hc) {
      hc.innerHTML = `
        <div class="kv-row"><span class="kv-key">Overall</span><span class="kv-value">${statusBadge(health.status)}</span></div>
        ${health.checks.map(c => `
          <div class="kv-row">
            <span class="kv-key">${esc(c.name)}</span>
            <span class="kv-value">${c.status === 'pass'
              ? '<span class="badge badge-success">Pass</span>'
              : '<span class="badge badge-danger">Fail</span>'}
              ${c.message ? `<span class="text-muted text-sm" style="margin-left:0.5rem">${esc(c.message)}</span>` : ''}
            </span>
          </div>
        `).join('')}
      `;
    }
  } catch (e) { toast(e.message, 'error'); }
}

// ── Settings ──────────────────────────────────────────────────────

async function renderSettings() {
  renderPage('settings');
  try {
    const info = await api('/node/info');
    const el = document.getElementById('node-config');
    if (el) {
      el.innerHTML = `
        <div class="kv-row"><span class="kv-key">NODE_ID</span><span class="kv-value">${esc(info.node_id)}</span></div>
        <div class="kv-row"><span class="kv-key">VERSION</span><span class="kv-value">${info.version}</span></div>
        <div class="kv-row"><span class="kv-key">UPTIME</span><span class="kv-value">${fmtDuration(info.uptime_secs)}</span></div>
        <div class="kv-row"><span class="kv-key">CONTAINERS</span><span class="kv-value">${info.containers_total} total, ${info.containers_running} running</span></div>
        <div class="kv-row"><span class="kv-key">CPU</span><span class="kv-value">${info.system.cpu_count} cores</span></div>
        <div class="kv-row"><span class="kv-key">MEMORY</span><span class="kv-value">${fmtBytes(info.system.used_memory_bytes)} / ${fmtBytes(info.system.total_memory_bytes)}</span></div>
        <div class="kv-row"><span class="kv-key">DISK</span><span class="kv-value">${fmtBytes(info.system.used_disk_bytes)} / ${fmtBytes(info.system.total_disk_bytes)}</span></div>
      `;
    }
  } catch (e) { toast(e.message, 'error'); }
  NX.checkForUpdates();
}

// ── Mod Marketplace ──────────────────────────────────────────────

async function renderMarketplace() {
  renderPage('marketplace');
  const searchInput = document.getElementById('mp-search');
  const gameFilter = document.getElementById('mp-game');
  const providerFilter = document.getElementById('mp-provider');
  const results = document.getElementById('mp-results');
  if (!searchInput || !results) return;

  async function doSearch() {
    const q = searchInput.value.trim();
    if (!q) { results.innerHTML = '<p class="text-muted">Enter a search term to find mods.</p>'; return; }
    results.innerHTML = '<p class="text-muted">Searching...</p>';
    try {
      const params = new URLSearchParams({ q, limit: '24' });
      if (gameFilter.value) params.set('game', gameFilter.value);
      if (providerFilter.value) params.set('provider', providerFilter.value);
      const mods = await api('/marketplace/search?' + params);
      if (!mods.length) { results.innerHTML = '<p class="text-muted">No mods found.</p>'; return; }
      results.innerHTML = mods.map(m => `
        <div class="blueprint-card" onclick="NX.viewMod('${esc(m.provider)}','${esc(m.id)}')">
          <div class="flex items-center gap-1" style="margin-bottom:0.5rem">
            <h3 style="margin:0;flex:1">${esc(m.name)}</h3>
            <span class="badge badge-info">${esc(m.provider)}</span>
          </div>
          <p class="text-sm text-muted" style="margin-bottom:0.5rem">${esc(m.description).slice(0, 120)}${m.description.length > 120 ? '...' : ''}</p>
          <div class="flex items-center gap-1 text-xs text-muted">
            <span>by ${esc(m.author)}</span>
            <span>&middot;</span>
            <span>${Number(m.downloads).toLocaleString()} downloads</span>
            ${m.rating ? `<span>&middot;</span><span>${m.rating.toFixed(1)}/5</span>` : ''}
          </div>
        </div>
      `).join('');
    } catch (e) { results.innerHTML = `<p class="text-danger">${esc(e.message)}</p>`; }
  }

  let debounce = null;
  searchInput.addEventListener('input', () => { clearTimeout(debounce); debounce = setTimeout(doSearch, 400); });
  gameFilter.addEventListener('change', doSearch);
  providerFilter.addEventListener('change', doSearch);
}

NX.viewMod = async function(provider, modId) {
  try {
    const mod = await api(`/marketplace/mods/${encodeURIComponent(provider)}/${encodeURIComponent(modId)}`);
    document.getElementById('modal-title').textContent = mod.name;
    document.getElementById('modal-body').innerHTML = `
      <div style="margin-bottom:1rem">
        <span class="badge badge-info">${esc(mod.provider)}</span>
        <span class="text-muted text-sm" style="margin-left:0.5rem">by ${esc(mod.author)}</span>
      </div>
      <p>${esc(mod.description)}</p>
      <div class="kv-row" style="margin-top:1rem"><span class="kv-key">Latest Version</span><span class="kv-value">${esc(mod.latest_version)}</span></div>
      <div class="kv-row"><span class="kv-key">Downloads</span><span class="kv-value">${Number(mod.downloads).toLocaleString()}</span></div>
      ${mod.rating ? `<div class="kv-row"><span class="kv-key">Rating</span><span class="kv-value">${mod.rating.toFixed(1)} / 5</span></div>` : ''}
      ${mod.url ? `<div style="margin-top:1rem"><a href="${esc(mod.url)}" target="_blank" rel="noopener" class="btn btn-sm">View on ${esc(mod.provider)}</a></div>` : ''}
      <div id="mod-install-section" style="margin-top:1.25rem;border-top:1px solid var(--border);padding-top:1rem"></div>
    `;
    document.getElementById('modal-overlay').classList.remove('hidden');

    // Populate the "Install to server" section with the current server list.
    const section = document.getElementById('mod-install-section');
    let servers = [];
    try { servers = await api('/containers'); } catch (_) { servers = []; }
    // Steam Workshop items are whole mod folders the game loads from the
    // server root (`@Mod` for DayZ/Arma, the Workshop id elsewhere), so they
    // have no plugin-framework choice to make.
    const workshop = mod.provider === 'steam_workshop';
    // Rust servers run either Oxide or Carbon, which use different plugin
    // folders. Minecraft-style mods just drop into /plugins.
    const rustLike = !workshop && mod.game !== 'minecraft';
    const defaultDir = workshop ? '.' : (rustLike ? 'oxide/plugins' : 'plugins');
    if (!servers.length) {
      section.innerHTML = '<p class="text-muted text-sm">Create a server first to install this mod.</p>';
    } else {
      const options = servers.map(c => `<option value="${esc(c.id)}">${esc(c.name)} (${esc(c.status)})</option>`).join('');
      const frameworkRow = rustLike ? `
        <div class="form-group">
          <label class="form-label">Framework</label>
          <select id="install-framework" class="form-input" style="width:100%" onchange="NX.onFrameworkChange()">
            <option value="oxide">Oxide / uMod (oxide/plugins)</option>
            <option value="carbon">Carbon (carbon/plugins)</option>
          </select>
        </div>` : '';
      const workshopNote = workshop ? `
        <p class="text-sm text-muted" style="margin-bottom:0.75rem">
          Downloaded with SteamCMD (or DepotDownloader) and installed as a mod
          folder inside this directory; for DayZ and Arma, signature keys are
          copied into <code>keys/</code> automatically. Those games also need
          <code>STEAM_USERNAME</code> configured on this node.
        </p>` : '';
      section.innerHTML = `
        <h4 style="margin-bottom:0.75rem">Install to server</h4>
        ${workshopNote}
        <div class="form-group">
          <label class="form-label">Server</label>
          <select id="install-server" class="form-input" style="width:100%">${options}</select>
        </div>
        ${frameworkRow}
        <div class="form-group">
          <label class="form-label">Install folder</label>
          <input id="install-dir" class="form-input" style="width:100%" value="${esc(defaultDir)}">
        </div>
        <button class="btn btn-primary" id="install-btn" onclick="NX.installMod('${esc(mod.provider)}','${esc(mod.id)}')">Install</button>
        <div id="install-status" class="text-sm" style="margin-top:0.6rem"></div>`;
    }
  } catch (e) { toast(e.message, 'error'); }
};

// Keep the install-folder input in sync when the operator switches framework,
// unless they've hand-edited it to something non-default.
NX.onFrameworkChange = function() {
  const fw = document.getElementById('install-framework')?.value;
  const dirEl = document.getElementById('install-dir');
  if (!fw || !dirEl) return;
  const known = ['oxide/plugins', 'carbon/plugins'];
  if (known.includes(dirEl.value.trim())) {
    dirEl.value = fw === 'carbon' ? 'carbon/plugins' : 'oxide/plugins';
  }
};

NX.installMod = async function(provider, modId) {
  const server = document.getElementById('install-server')?.value;
  const dir = (document.getElementById('install-dir')?.value || '').trim();
  const framework = document.getElementById('install-framework')?.value;
  const status = document.getElementById('install-status');
  const btn = document.getElementById('install-btn');
  if (!server) { status.innerHTML = '<span class="text-danger">Select a server.</span>'; return; }
  status.innerHTML = '<span class="text-muted">Starting install…</span>';
  btn.disabled = true;
  try {
    const body = { provider, mod_id: modId, target_dir: dir };
    if (framework) body.framework = framework;
    // The install runs as a background job — a Workshop mod can be gigabytes,
    // far longer than a request should be held open.
    const job = await api(`/containers/${encodeURIComponent(server)}/mods/install`, {
      method: 'POST',
      body: JSON.stringify(body),
    });
    NX.renderInstallJob(server, job);
  } catch (e) {
    status.innerHTML = `<span class="text-danger">${esc(e.message)}</span>`;
    btn.disabled = false;
  }
};

// Show a mod-install job's state, re-polling while it runs. The modal can be
// closed and the install continues on the node regardless.
NX.renderInstallJob = function(server, job) {
  const status = document.getElementById('install-status');
  const btn = document.getElementById('install-btn');
  if (!status) return; // Operator closed the dialog; the job runs on.

  if (job.status === 'running') {
    status.innerHTML = '<span class="text-muted">Downloading… large mods can take several '
      + 'minutes. You can leave this dialog open or check back later.</span>';
    if (btn) btn.disabled = true;
    clearTimeout(NX.installPollTimer);
    NX.installPollTimer = setTimeout(async () => {
      try {
        const next = await api(`/containers/${encodeURIComponent(server)}/mods/install`);
        NX.renderInstallJob(server, next);
      } catch (_) {}
    }, 2000);
    return;
  }

  if (btn) btn.disabled = false;

  if (job.status === 'succeeded') {
    const keys = (job.signature_keys || []).length;
    const keyNote = keys
      ? `<div class="text-sm text-muted" style="margin-top:0.35rem">Installed ${keys} signature key${keys === 1 ? '' : 's'} to <code>keys/</code> — clients can connect with signature verification on.</div>`
      : '';
    status.innerHTML = `<span class="text-success">Installed → ${esc(job.file_path || '')} (${fmtBytes(job.file_size || 0)})</span>${keyNote}`;
    toast('Mod installed', 'success');
  } else {
    status.innerHTML = `<span class="text-danger">${esc(job.error || 'Install failed')}</span>`;
  }
};

// ── Update check ─────────────────────────────────────────────────

NX.checkForUpdates = async function() {
  const el = document.getElementById('node-update');
  if (!el) return;
  el.innerHTML = '<span class="text-muted">Checking for updates…</span>';

  try {
    const info = await api('/node/update-check');
    NX.nodeUpdate = info;
    const c = info.current;
    const built = `v${esc(c.version)}`
      + (c.commit_short && c.commit_short !== 'unknown' ? ` · <code>${esc(c.commit_short)}</code>` : '')
      + (c.dirty ? ' <span class="badge badge-warning">modified</span>' : '');

    let headline;
    if (info.error) {
      // Could not reach a conclusion. Saying "up to date" here would be a
      // guess, and the wrong one to guess.
      headline = `<span class="badge badge-secondary" style="margin-right:0.5rem">Unknown</span>`
        + `<span class="text-muted">${esc(info.error)}</span>`;
    } else if (info.update_available) {
      const behind = info.commits_behind
        ? ` — ${info.commits_behind} commit${info.commits_behind === 1 ? '' : 's'} behind`
        : '';
      headline = `<span class="badge badge-warning" style="margin-right:0.5rem">Update available</span>`
        + `<span>${esc(info.latest)} on the <strong>${esc(info.channel)}</strong> channel${esc(behind)}</span>`;
    } else {
      headline = `<span class="badge badge-success" style="margin-right:0.5rem">Up to date</span>`
        + `<span class="text-muted">on the <strong>${esc(info.channel)}</strong> channel</span>`;
    }

    el.innerHTML = `${headline}
      <div class="text-muted text-sm" style="margin-top:0.5rem">Running ${built}</div>`;

    NX.renderNodeUpdateActions();
  } catch (e) {
    el.innerHTML = `<span class="text-muted">Could not check for updates: ${esc(e.message)}</span>`;
  }

  NX.refreshNodeUpdateJob();
};

// The Update button, shown only when there is something to apply.
NX.renderNodeUpdateActions = function() {
  const box = document.getElementById('node-update-actions');
  if (!box) return;
  const info = NX.nodeUpdate;
  const applicable = info && info.update_available;
  box.innerHTML = `
    <button class="btn btn-primary" id="node-update-run" onclick="NX.applyNodeUpdate()"
      ${applicable ? '' : 'disabled'}>Update panel</button>
    <span class="text-muted text-sm" style="margin-left:0.75rem">
      ${applicable
        ? 'Rebuilds and restarts the node. Running game servers keep running; the panel is briefly unavailable.'
        : 'Nothing to apply.'}
    </span>`;
};

NX.applyNodeUpdate = async function() {
  if (!confirm('Update the panel now?\n\nThe node rebuilds and restarts, so the panel will be '
    + 'unavailable for a few minutes. Running game servers are not affected. If the new build '
    + 'fails to start, the previous one is restored automatically.')) return;

  const btn = document.getElementById('node-update-run');
  if (btn) btn.disabled = true;
  try {
    const job = await api('/node/update', { method: 'POST' });
    toast('Update started', 'success');
    NX.renderNodeUpdateJob(job);
    NX.pollNodeUpdate();
  } catch (e) {
    toast(e.message, 'error');
    if (btn) btn.disabled = false;
  }
};

NX.refreshNodeUpdateJob = async function() {
  try {
    const job = await api('/node/update');
    NX.renderNodeUpdateJob(job);
    if (job.status === 'running') NX.pollNodeUpdate();
  } catch (_) {
    // Never updated from the panel — nothing to show.
  }
};

NX.renderNodeUpdateJob = function(job) {
  const box = document.getElementById('node-update-job');
  const log = document.getElementById('node-update-log');
  if (!box) return;

  const badge = {
    running: '<span class="badge badge-warning">Updating…</span>',
    succeeded: '<span class="badge badge-success">Updated</span>',
    failed: '<span class="badge badge-danger">Update failed</span>',
    rolled_back: '<span class="badge badge-danger">Rolled back</span>',
  }[job.status] || esc(job.status);

  let line = `${badge} <span class="text-muted text-sm">from v${esc(job.from_version)} · ${esc(job.channel)} channel</span>`;
  if (job.error) line += `<div class="text-danger text-sm" style="margin-top:0.35rem">${esc(job.error)}</div>`;
  if (job.status === 'rolled_back') {
    line += '<div class="text-muted text-sm" style="margin-top:0.35rem">The previous version was '
      + 'restored, so this node is still running — but it is still on the old build.</div>';
  }
  box.innerHTML = line;

  if (job.log && log) {
    log.textContent = job.log;
    log.classList.remove('hidden');
    log.scrollTop = log.scrollHeight;
  }

  const btn = document.getElementById('node-update-run');
  if (btn) btn.disabled = job.status === 'running';
};

// Follow an update across the restart it causes.
//
// The node goes away part-way through — that is the update working, not a
// failure — so a failed poll is retried rather than surfaced. Only once the
// node answers again does its status file decide the outcome.
NX.pollNodeUpdate = function() {
  clearTimeout(NX.nodeUpdatePollTimer);
  NX.nodeUpdatePollTimer = setTimeout(async () => {
    try {
      const job = await api('/node/update');
      NX.nodeUpdateUnreachable = 0;
      NX.renderNodeUpdateJob(job);
      if (job.status === 'running') {
        NX.pollNodeUpdate();
      } else {
        // Whatever it is now, the version banner is stale.
        NX.checkForUpdates();
      }
    } catch (_) {
      NX.nodeUpdateUnreachable = (NX.nodeUpdateUnreachable || 0) + 1;
      const box = document.getElementById('node-update-job');
      if (box) {
        box.innerHTML = '<span class="badge badge-warning">Restarting…</span>'
          + '<span class="text-muted text-sm" style="margin-left:0.5rem">'
          + 'The node is restarting into the new build. This page reconnects on its own.</span>';
      }
      // Roughly ten minutes of a node that never comes back is long enough to
      // stop pretending it is coming back.
      if (NX.nodeUpdateUnreachable < 200) {
        NX.pollNodeUpdate();
      } else if (box) {
        box.innerHTML = '<span class="badge badge-danger">Node did not come back</span>'
          + '<span class="text-muted text-sm" style="margin-left:0.5rem">'
          + 'Check <code>journalctl -u nexus-node -n 50</code> on the host.</span>';
      }
    }
  }, 3000);
};

// ── Modal ─────────────────────────────────────────────────────────

NX.closeModal = function() {
  document.getElementById('modal-overlay').classList.add('hidden');
};

// ── Utility ───────────────────────────────────────────────────────

// HTML-escape for both text and attribute context. The textContent trick
// leaves quotes alone, and most of this file interpolates into single-quoted
// onclick attributes — a file named x');alert(1);(' broke out of them.
function esc(s) {
  return String(s ?? '').replace(/[&<>"'`]/g, c => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;', '`': '&#96;',
  })[c]);
}

function statusBadge(status) {
  const map = {
    running:   ['success', 'Online'],
    created:   ['info',    'Created'],
    stopped:   ['muted',   'Stopped'],
    failed:    ['danger',  'Failed'],
    suspended: ['warning', 'Suspended'],
    paused:    ['warning', 'Paused'],
    healthy:   ['success', 'Healthy'],
    degraded:  ['warning', 'Degraded'],
    unhealthy: ['danger',  'Unhealthy'],
    completed: ['success', 'Completed'],
    pending:   ['info',    'Pending'],
    error:     ['danger',  'Error'],
  };
  const [cls, label] = map[status] || ['muted', status];
  return `<span class="badge badge-${cls}"><span class="badge-dot"></span>${label}</span>`;
}

function fmtBytes(b) {
  if (b === 0) return '0 B';
  const units = ['B','KB','MB','GB','TB'];
  const i = Math.floor(Math.log(b) / Math.log(1024));
  return (b / Math.pow(1024, i)).toFixed(i > 0 ? 1 : 0) + ' ' + units[i];
}

function fmtTime(epoch) {
  if (!epoch) return '—';
  return new Date(epoch * 1000).toLocaleDateString();
}

function fmtDuration(secs) {
  if (!secs || secs < 0) return '—';
  const d = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}

// ── Login / boot ──────────────────────────────────────────────────

function showLogin(message) {
  document.body.classList.add('login-mode');
  let overlay = document.getElementById('login-overlay');
  if (!overlay) {
    overlay = document.createElement('div');
    overlay.id = 'login-overlay';
    overlay.className = 'login-overlay';
    overlay.innerHTML = `
      <div class="login-stars"></div>
      <div class="login-card">
        <div class="login-logo">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="40" height="40">
            <polygon points="12 2 22 8.5 22 15.5 12 22 2 15.5 2 8.5"/>
            <polyline points="22 8.5 12 15.5 2 8.5"/><line x1="12" y1="2" x2="12" y2="8.5"/>
          </svg>
        </div>
        <h1 class="login-title">Nexus Panel</h1>
        <p class="login-sub">Sign in to your control panel</p>
        <form id="login-form" class="login-form" autocomplete="off">
          <input type="password" id="login-password" class="login-input" placeholder="Admin password" autofocus>
          <button type="submit" class="btn btn-primary login-btn">Sign In</button>
          <div id="login-error" class="login-error"></div>
        </form>
      </div>`;
    document.body.appendChild(overlay);
    overlay.querySelector('#login-form').addEventListener('submit', submitLogin);
  }
  overlay.style.display = 'flex';
  const err = overlay.querySelector('#login-error');
  err.textContent = message || '';
  const input = overlay.querySelector('#login-password');
  input.value = '';
  input.focus();
}

function hideLogin() {
  document.body.classList.remove('login-mode');
  const overlay = document.getElementById('login-overlay');
  if (overlay) overlay.style.display = 'none';
}

async function submitLogin(e) {
  e.preventDefault();
  const input = document.getElementById('login-password');
  const err = document.getElementById('login-error');
  const btn = e.target.querySelector('button[type=submit]');
  err.textContent = '';
  btn.disabled = true;
  btn.textContent = 'Signing in…';
  try {
    const res = await fetch(API + '/auth/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ password: input.value }),
    });
    if (!res.ok) {
      const body = await res.json().catch(() => ({ error: res.statusText }));
      throw new Error(body.error || 'Invalid credentials');
    }
    const data = await res.json();
    Auth.token = data.token;
    hideLogin();
    route();
  } catch (ex) {
    err.textContent = ex.message || 'Sign in failed';
    input.focus();
  } finally {
    btn.disabled = false;
    btn.textContent = 'Sign In';
  }
}

async function logout() {
  const wasCustomer = NX.scope !== 'admin';
  try {
    await fetch(API + '/auth/logout', {
      method: 'POST',
      headers: Auth.token ? { 'Authorization': 'Bearer ' + Auth.token } : {},
    });
  } catch (_) { /* best effort */ }
  Auth.clear();
  applyScope('admin', []);
  showLogin(wasCustomer
    ? 'You have been signed out. Open the panel from your billing portal to sign in again.'
    : 'You have been signed out.');
}
NX.logout = logout;

// Shape the UI to what this session may do. A customer sees their servers
/// and nothing node-level; the node-level pages would only 403.
function applyScope(scope, serverIds) {
  NX.scope = scope;
  NX.serverIds = serverIds || [];
  document.body.classList.toggle('scope-servers', scope !== 'admin');
  if (scope !== 'admin') {
    document.getElementById('sidebar-node-id').textContent = 'My servers';
    document.getElementById('sidebar-version').textContent = '';
  }
}

async function boot() {
  // Ask the node whether authentication is required.
  let authRequired = false;
  try {
    const cfg = await (await fetch(API + '/auth/config')).json();
    authRequired = !!cfg.auth_required;
  } catch (_) {
    authRequired = false;
  }

  if (authRequired) {
    // Whoever we are — a stored admin token, or the cookie a billing-portal
    // sign-in set — the node says so. A 401 here shows the login screen.
    let me;
    try {
      me = await api('/auth/me');
    } catch (_) {
      if (!document.body.classList.contains('login-mode')) showLogin();
      return;
    }
    applyScope(me.scope, me.server_ids);
  } else {
    applyScope('admin', []);
  }

  hideLogin();
  route();
}

// ── Init ──────────────────────────────────────────────────────────

window.addEventListener('hashchange', () => {
  if (!document.body.classList.contains('login-mode')) route();
});
document.addEventListener('DOMContentLoaded', boot);
