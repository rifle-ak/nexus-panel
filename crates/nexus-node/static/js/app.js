/* ═══════════════════════════════════════════════════════════════════
   Nexus Panel – Frontend SPA
   Vanilla JS, no build step, no framework.
   ═══════════════════════════════════════════════════════════════════ */

const API = '/api/v1';

// ── Global state ──────────────────────────────────────────────────

const NX = window.NX = {
  containers: [],
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
  const res = await fetch(API + path, {
    headers: { 'Content-Type': 'application/json', ...opts.headers },
    ...opts,
  });
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
  const page = parts[0] || 'dashboard';

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
        <span class="status-item-label">Image</span>
        <span class="status-item-value text-sm">${esc(c.image)}</span>
      </div>
      <div class="status-item">
        <span class="status-item-label">PID</span>
        <span class="status-item-value">${c.pid || '—'}</span>
      </div>
      <div class="status-item">
        <span class="status-item-label">Restarts</span>
        <span class="status-item-value">${c.restart_count}</span>
      </div>
      <div class="status-item">
        <span class="status-item-label">Uptime</span>
        <span class="status-item-value">${c.started_at ? fmtDuration(Date.now()/1000 - c.started_at) : '—'}</span>
      </div>
    `;

    // Action buttons
    document.getElementById('detail-actions').innerHTML = `
      ${c.status === 'running'
        ? `<button class="btn btn-sm btn-danger" onclick="NX.stopServer('${id}')">Stop</button>
           <button class="btn btn-sm" onclick="NX.restartServer('${id}')">Restart</button>
           <button class="btn btn-sm btn-danger" onclick="NX.deleteServer('${id}')">Delete</button>`
        : `<button class="btn btn-sm btn-success" onclick="NX.startServer('${id}')">Start</button>
           <button class="btn btn-sm btn-danger" onclick="NX.deleteServer('${id}')">Delete</button>`
      }
    `;

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
    NX.refreshTimer = setInterval(async () => {
      try {
        const updated = await api(`/containers/${id}`);
        document.getElementById('detail-status-bar').querySelector('.status-item-value').innerHTML = statusBadge(updated.status);
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
    const res = await fetch(`${API}/containers/${NX.currentServer}/files/read?path=${encodeURIComponent(path)}`);
    if (!res.ok) throw new Error(await res.text());
    const text = await res.text();

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

NX.showCreateServer = function() {
  document.getElementById('modal-title').textContent = 'Create Server';
  document.getElementById('modal-body').innerHTML = `
    <div class="form-group">
      <label class="form-label">Blueprint Config (YAML)</label>
      <textarea class="form-input" id="create-yaml" style="width:100%;height:280px" placeholder="Paste your blueprint YAML here...">metadata:
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
      protocol: tcp</textarea>
    </div>
    <div class="form-group">
      <label class="form-label">
        <input type="checkbox" id="create-autostart" checked> Auto-start after creation
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
    toast('Server created: ' + res.id, 'success');
    NX.closeModal();
    location.hash = '#/servers/' + res.id;
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

NX.deployBlueprint = async function(bpId) {
  // In a real deployment, this would fetch the blueprint YAML from /blueprints/{id}.yaml
  toast(`Blueprint "${bpId}" selected. Use Create Server with the matching YAML.`, 'info');
  NX.showCreateServer();
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
}

// ── Modal ─────────────────────────────────────────────────────────

NX.closeModal = function() {
  document.getElementById('modal-overlay').classList.add('hidden');
};

// ── Utility ───────────────────────────────────────────────────────

function esc(s) {
  const d = document.createElement('div');
  d.textContent = s;
  return d.innerHTML;
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

// ── Init ──────────────────────────────────────────────────────────

window.addEventListener('hashchange', route);
document.addEventListener('DOMContentLoaded', route);
