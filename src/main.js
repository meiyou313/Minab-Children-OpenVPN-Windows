// Minab OpenVPN — Windows (Tauri v2) frontend logic.
// Vanilla JS, no framework. Talks to the Rust backend through the
// Tauri v2 command / event contract:
//
//   Commands: list_profiles, import_config_texts, start_ping_test,
//             connect, disconnect, get_status, delete_profile,
//             set_favorite, set_group, get_usage
//   Events:   "vpn-state", "ping-result", "bytecount"
//
// If the page runs outside Tauri (plain browser preview) every backend
// call fails gracefully with a toast instead of crashing.

const $ = (sel) => document.querySelector(sel);
const $$ = (sel) => [...document.querySelectorAll(sel)];

// ---------------------------------------------------------------- state
let profiles = []; // Profile[]
let status = { state: "disconnected", profileId: null, bytesIn: 0, bytesOut: 0, message: "" };
let selectedProfileId = null;
let connectingId = null;
let activeGroupFilter = "__all__";
let graphPeriod = "today"; // today | week | month
let usage = { todayBytes: 0, weekBytes: 0, monthBytes: 0 };
let pendingFiles = []; // { name, text } picked in Import, awaiting import
let statusPollFailedOnce = false;

// Real-test settings (persisted locally; mirrors Android defaults).
const settings = {
  waitSeconds: Number(localStorage.getItem("minab.waitSeconds") || 12),
  healthyServers: Number(localStorage.getItem("minab.healthyServers") || 5),
  testWithoutPing: localStorage.getItem("minab.testWithoutPing") === "true",
};

// ---------------------------------------------------------------- tauri
function tauriAvailable() {
  return Boolean(window.__TAURI__?.core?.invoke);
}

async function invoke(cmd, args) {
  if (!tauriAvailable()) throw new Error("Tauri backend is not available in this window.");
  return window.__TAURI__.core.invoke(cmd, args ?? {});
}

async function listen(eventName, handler) {
  if (!window.__TAURI__?.event?.listen) return;
  try {
    await window.__TAURI__.event.listen(eventName, (event) => handler(event.payload ?? {}));
  } catch (err) {
    console.warn("Could not listen for", eventName, err);
  }
}

// ---------------------------------------------------------------- helpers
function toast(message, ms = 3500) {
  const el = $("#toast");
  el.textContent = message;
  el.hidden = false;
  clearTimeout(toast._timer);
  toast._timer = setTimeout(() => {
    el.hidden = true;
  }, ms);
}

function formatBytes(n) {
  const v = Number(n) || 0;
  if (v < 1024) return `${v} B`;
  if (v < 1024 * 1024) return `${(v / 1024).toFixed(1)} KB`;
  if (v < 1024 * 1024 * 1024) return `${(v / (1024 * 1024)).toFixed(1)} MB`;
  return `${(v / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

function flagEmoji(countryCode) {
  const code = String(countryCode || "").trim().toUpperCase();
  if (!/^[A-Z]{2}$/.test(code)) return "🌐";
  return [...code]
    .map((ch) => String.fromCodePoint(0x1f1e6 + ch.charCodeAt(0) - 65))
    .join("");
}

function stateLabel(state) {
  switch (String(state || "").toLowerCase()) {
    case "connected":
      return "Connected";
    case "connecting":
      return "Connecting…";
    case "disconnecting":
      return "Disconnecting…";
    default:
      return "Disconnected";
  }
}

function profileById(id) {
  return profiles.find((p) => p.id === id) || null;
}

function groupNameOf(p) {
  return (p.group || "").trim() || "Ungrouped";
}

// ---------------------------------------------------------------- profiles
async function loadProfiles() {
  try {
    const list = await invoke("list_profiles");
    profiles = Array.isArray(list) ? list : [];
  } catch (err) {
    console.warn("list_profiles failed:", err);
    toast(`Could not load servers: ${err.message || err}`);
    profiles = [];
  }
  renderHome();
  renderConfigs();
  renderGroupControls();
}

function sortedGroups() {
  const groups = [...new Set(profiles.map(groupNameOf))];
  return groups.sort((a, b) => a.localeCompare(b));
}

function buildServerRow(p, { showDelete = false } = {}) {
  const row = document.createElement("div");
  row.className = "server-row";
  row.dataset.profileId = p.id;
  if (status.profileId === p.id && status.state !== "disconnected") row.classList.add("is-active");
  if (connectingId === p.id) row.classList.add("is-connecting");

  const flag = document.createElement("span");
  flag.className = "row-flag";
  flag.textContent = flagEmoji(p.countryCode);
  row.appendChild(flag);

  const main = document.createElement("button");
  main.type = "button";
  main.className = "row-main-btn";
  main.style.cssText =
    "flex:1;min-width:0;border:none;background:none;padding:0;text-align:left;color:inherit;font:inherit;cursor:pointer;";
  const name = document.createElement("span");
  name.className = "row-name";
  name.textContent = p.name;
  const meta = document.createElement("span");
  meta.className = "row-meta";
  meta.textContent = `${groupNameOf(p)} · ${p.remoteHost || ""}${p.remotePort ? ":" + p.remotePort : ""} · ${String(p.proto || "").toUpperCase() || "UDP"}`;
  main.append(name, meta);
  main.addEventListener("click", () => connectTo(p.id));
  row.appendChild(main);

  const badges = document.createElement("div");
  badges.className = "row-badges";
  if (connectingId === p.id) {
    const spin = document.createElement("span");
    spin.className = "row-spinner";
    spin.setAttribute("aria-label", "Connecting");
    badges.appendChild(spin);
  }
  const ping = document.createElement("span");
  ping.className = "badge badge-ping";
  ping.dataset.pingFor = p.id;
  ping.textContent = pingBadgeText(p);
  if (typeof p.pingMs === "number" && p.pingMs > 300) ping.classList.add("bad");
  badges.appendChild(ping);
  row.appendChild(badges);

  const actions = document.createElement("div");
  actions.className = "row-actions";

  const pingBtn = document.createElement("button");
  pingBtn.type = "button";
  pingBtn.className = "ping-btn";
  pingBtn.textContent = "Ping";
  pingBtn.setAttribute("aria-label", `Ping ${p.name}`);
  pingBtn.addEventListener("click", (e) => {
    e.stopPropagation();
    startPing([p.id]);
  });
  actions.appendChild(pingBtn);

  const star = document.createElement("button");
  star.type = "button";
  star.className = "star-btn" + (p.favorite ? " is-fav" : "");
  star.textContent = p.favorite ? "★" : "☆";
  star.setAttribute("aria-label", p.favorite ? `Remove ${p.name} from favorites` : `Add ${p.name} to favorites`);
  star.addEventListener("click", async (e) => {
    e.stopPropagation();
    await toggleFavorite(p);
  });
  actions.appendChild(star);

  if (showDelete) {
    const del = document.createElement("button");
    del.type = "button";
    del.className = "ping-btn";
    del.textContent = "✕";
    del.setAttribute("aria-label", `Delete ${p.name}`);
    del.addEventListener("click", async (e) => {
      e.stopPropagation();
      if (!window.confirm(`Delete "${p.name}"?`)) return;
      try {
        await invoke("delete_profile", { id: p.id });
        toast(`Deleted "${p.name}".`);
        await loadProfiles();
      } catch (err) {
        toast(`Could not delete: ${err.message || err}`);
      }
    });
    actions.appendChild(del);
  }

  row.appendChild(actions);
  return row;
}

function pingBadgeText(p) {
  if (typeof p.pingMs === "number" && p.pingMs >= 0) return `${p.pingMs} ms`;
  return "—";
}

function renderHome() {
  const list = $("#home-list");
  list.innerHTML = "";
  const empty = $("#home-empty");

  if (!profiles.length) {
    empty.hidden = false;
    list.appendChild(empty);
    return;
  }
  empty.hidden = true;

  // Favorites section first, then the rest grouped by group name.
  const favs = profiles.filter((p) => p.favorite);
  const rest = profiles.filter((p) => !p.favorite);

  if (favs.length) {
    list.appendChild(groupHeading("★ Favorites"));
    favs.forEach((p) => list.appendChild(buildServerRow(p)));
  }

  const groups = [...new Set(rest.map(groupNameOf))].sort((a, b) => a.localeCompare(b));
  for (const g of groups) {
    list.appendChild(groupHeading(g));
    rest
      .filter((p) => groupNameOf(p) === g)
      .forEach((p) => list.appendChild(buildServerRow(p)));
  }
}

function groupHeading(text) {
  const h = document.createElement("h3");
  h.className = "group-heading";
  h.textContent = text;
  return h;
}

function renderConfigs() {
  const list = $("#configs-list");
  list.innerHTML = "";
  let visible = profiles;
  if (activeGroupFilter !== "__all__") {
    visible = profiles.filter((p) => groupNameOf(p) === activeGroupFilter);
  }
  if (!visible.length) {
    const note = document.createElement("p");
    note.className = "empty-note";
    note.textContent = profiles.length ? "No configs in this group." : "No configs yet.";
    list.appendChild(note);
    return;
  }
  visible.forEach((p) => list.appendChild(buildServerRow(p, { showDelete: true })));
}

function renderGroupControls() {
  // Configs filter chips.
  const chips = $("#group-chips");
  chips.innerHTML = "";
  const all = document.createElement("button");
  all.type = "button";
  all.className = "chip" + (activeGroupFilter === "__all__" ? " chip-active" : "");
  all.dataset.group = "__all__";
  all.textContent = "All";
  all.addEventListener("click", () => {
    activeGroupFilter = "__all__";
    renderGroupControls();
    renderConfigs();
  });
  chips.appendChild(all);
  for (const g of sortedGroups()) {
    const chip = document.createElement("button");
    chip.type = "button";
    chip.className = "chip" + (activeGroupFilter === g ? " chip-active" : "");
    chip.dataset.group = g;
    chip.textContent = g;
    chip.addEventListener("click", () => {
      activeGroupFilter = g;
      renderGroupControls();
      renderConfigs();
    });
    chips.appendChild(chip);
  }

  // Import group picker (datalist: pick an existing group or type a new one).
  const dataList = $("#group-options");
  if (dataList) {
    dataList.innerHTML = "";
    for (const g of sortedGroups()) {
      if (g === "Ungrouped") continue;
      const opt = document.createElement("option");
      opt.value = g;
      dataList.appendChild(opt);
    }
  }
}

async function toggleFavorite(p) {
  try {
    await invoke("set_favorite", { id: p.id, favorite: !p.favorite });
    p.favorite = !p.favorite;
    renderHome();
    renderConfigs();
  } catch (err) {
    toast(`Could not update favorite: ${err.message || err}`);
  }
}

// ---------------------------------------------------------------- tabs
function switchTab(name) {
  $$(".tab-btn").forEach((btn) => btn.classList.toggle("tab-active", btn.dataset.tab === name));
  $$(".tab-panel").forEach((panel) => {
    const isActive = panel.id === `tab-${name}`;
    panel.classList.toggle("active", isActive);
    panel.hidden = !isActive;
  });
  if (name === "graph") refreshUsage();
  if (name === "configs") renderConfigs();
}

// ---------------------------------------------------------------- connect
async function connectTo(profileId) {
  const p = profileById(profileId);
  selectedProfileId = profileId;
  connectingId = profileId;
  renderHome();
  renderConfigs();
  updateStatusUI({ ...status, state: "connecting", profileId: profileId });
  try {
    await invoke("connect", { profile_id: profileId });
  } catch (err) {
    connectingId = null;
    renderHome();
    renderConfigs();
    toast(`Could not connect${p ? ` to "${p.name}"` : ""}: ${err.message || err}`);
    await refreshStatus();
  }
}

async function disconnect() {
  try {
    await invoke("disconnect");
  } catch (err) {
    toast(`Could not disconnect: ${err.message || err}`);
  }
  await refreshStatus();
}

function onVpnState(payload) {
  const state = String(payload.state || "").toLowerCase();
  status = {
    state,
    profileId: payload.profileId ?? status.profileId,
    bytesIn: status.bytesIn,
    bytesOut: status.bytesOut,
    message: payload.message || "",
  };
  if (state !== "connecting") connectingId = null;
  if (state === "connected" && status.profileId) selectedProfileId = status.profileId;
  updateStatusUI(status);
  renderHome();
  renderConfigs();
  if (state === "connected") {
    const p = profileById(status.profileId);
    if (p) toast(`Connected to "${p.name}".`);
  } else if (state === "disconnected") {
    toast("Disconnected.");
  }
}

function onPingResult(payload) {
  const p = profileById(payload.profileId);
  if (!p) return;
  p.pingMs = payload.ok ? payload.ms : null;
  p.pingAt = Date.now();
  $$(`[data-ping-for="${CSS.escape(payload.profileId)}"]`).forEach((badge) => {
    badge.textContent = payload.ok ? `${payload.ms} ms` : "—";
    badge.classList.toggle("bad", Boolean(payload.ok) && payload.ms > 300);
  });
}

function onBytecount(payload) {
  status.bytesIn = Number(payload.bytesIn ?? status.bytesIn) || 0;
  status.bytesOut = Number(payload.bytesOut ?? status.bytesOut) || 0;
  $("#bytes-in").textContent = formatBytes(status.bytesIn);
  $("#bytes-out").textContent = formatBytes(status.bytesOut);
}

// ---------------------------------------------------------------- status
function updateStatusUI(s) {
  const state = String(s.state || "disconnected").toLowerCase();
  const label = stateLabel(state);
  $("#status-label").textContent = label;
  $("#status-detail").textContent =
    s.message || (state === "connected" ? "VPN is active" : state === "connecting" ? "Establishing a secure tunnel…" : "No server connected");
  $("#bytes-in").textContent = formatBytes(s.bytesIn);
  $("#bytes-out").textContent = formatBytes(s.bytesOut);

  const p = s.profileId ? profileById(s.profileId) : null;
  $("#status-server").textContent = p ? `${flagEmoji(p.countryCode)} ${p.name}` : "";

  const pill = $("#header-state");
  pill.textContent = label;
  pill.className = `state-pill state-${state === "connected" ? "connected" : state === "connecting" ? "connecting" : "disconnected"}`;

  const dot = $("#status-dot");
  dot.className = `status-dot dot-${state === "connected" ? "connected" : state === "connecting" ? "connecting" : "disconnected"}`;

  const btn = $("#connect-btn");
  btn.classList.toggle("is-connecting", state === "connecting");
  btn.classList.toggle("is-connected", state === "connected");
  $("#connect-label").textContent =
    state === "connected" ? "Disconnect" : state === "connecting" ? "Connecting…" : "Connect";
  btn.querySelector(".connect-icon").innerHTML =
    state === "connecting"
      ? '<span class="spinner" aria-hidden="true"></span>'
      : `<svg viewBox="0 0 24 24" width="26" height="26" fill="none">
           <path d="M12 3v10" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" />
           <path d="M6.5 6.5a8 8 0 1 0 11 0" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" />
         </svg>`;
}

async function refreshStatus() {
  try {
    const s = await invoke("get_status");
    if (s && typeof s === "object") {
      status = {
        state: String(s.state || "disconnected").toLowerCase(),
        profileId: s.profileId ?? null,
        bytesIn: Number(s.bytesIn) || 0,
        bytesOut: Number(s.bytesOut) || 0,
        message: s.message || "",
      };
      if (status.state !== "connecting") connectingId = null;
      updateStatusUI(status);
      renderHome();
      renderConfigs();
    }
  } catch (err) {
    if (!statusPollFailedOnce) {
      statusPollFailedOnce = true;
      toast(`Could not read status: ${err.message || err}`);
    }
    // Keep the last known status; the next poll retries quietly.
  }
}

// ---------------------------------------------------------------- ping
async function startPing(profileIds) {
  try {
    await invoke("start_ping_test", { profile_ids: profileIds ?? null });
    toast(profileIds ? "Ping test started." : "Testing all servers…");
  } catch (err) {
    toast(`Could not start ping test: ${err.message || err}`);
  }
}

// ---------------------------------------------------------------- graph
async function refreshUsage() {
  try {
    const u = await invoke("get_usage");
    if (u && typeof u === "object") usage = u;
  } catch (err) {
    toast(`Could not load usage: ${err.message || err}`);
  }
  renderGraph();
}

function renderGraph() {
  // The backend returns only three totals (today / week / month), so the
  // chart shows those three totals as bars — no per-day series is invented.
  const totals = [
    { key: "today", label: "Today", value: Number(usage.todayBytes) || 0 },
    { key: "week", label: "Week", value: Number(usage.weekBytes) || 0 },
    { key: "month", label: "Month", value: Number(usage.monthBytes) || 0 },
  ];
  const max = Math.max(...totals.map((t) => t.value), 1);
  const current = totals.find((t) => t.key === graphPeriod) || totals[0];

  $("#graph-total-value").textContent = formatBytes(current.value);
  $("#graph-total-label").textContent =
    current.key === "today" ? "Today" : current.key === "week" ? "This week" : "This month";
  $("#graph-footer-text").textContent = `Total ${formatBytes(current.value)} · ${$("#graph-total-label").textContent}`;

  const bars = $("#graph-bars");
  const labels = $("#graph-bar-labels");
  bars.innerHTML = "";
  labels.innerHTML = "";
  for (const t of totals) {
    const bar = document.createElement("div");
    bar.className = "graph-bar" + (t.key === graphPeriod ? " bar-current" : "");
    bar.style.height = `${Math.max(4, Math.round((t.value / max) * 100))}%`;
    bar.title = `${t.label}: ${formatBytes(t.value)}`;
    bars.appendChild(bar);
    const lab = document.createElement("span");
    lab.textContent = t.label;
    labels.appendChild(lab);
  }
}

// ---------------------------------------------------------------- import
function readFilesAsText(files) {
  return Promise.all(
    [...files].map(
      (file) =>
        new Promise((resolve, reject) => {
          const reader = new FileReader();
          reader.onload = () => resolve({ name: file.name, text: String(reader.result || "") });
          reader.onerror = () => reject(new Error(`Could not read ${file.name}`));
          reader.readAsText(file);
        })
    )
  );
}

async function doImport() {
  if (!pendingFiles.length) {
    toast("Choose at least one .ovpn file first.");
    return;
  }
  const username = $("#import-username").value.trim();
  const password = $("#import-password").value;
  const group = $("#import-group").value.trim();

  const resultEl = $("#import-result");
  resultEl.className = "import-result";
  resultEl.textContent = `Importing ${pendingFiles.length} file(s)…`;
  try {
    const res = await invoke("import_config_texts", {
      contents: pendingFiles.map((f) => f.text),
      names: pendingFiles.map((f) => f.name),
      username,
      password,
      group: group || null,
    });
    const imported = Number(res?.imported) || 0;
    const errors = Array.isArray(res?.errors) ? res.errors : [];
    resultEl.classList.add(errors.length ? "error" : "ok");
    resultEl.textContent =
      `Imported ${imported} config(s).` + (errors.length ? ` ${errors.length} error(s): ${errors.join("; ")}` : "");
    if (imported > 0) {
      pendingFiles = [];
      $("#ovpn-files").value = "";
      updateFileCount();
      await loadProfiles();
    }
  } catch (err) {
    resultEl.classList.add("error");
    resultEl.textContent = `Import failed: ${err.message || err}`;
  }
}

function updateFileCount() {
  const el = $("#file-count");
  if (pendingFiles.length) {
    el.hidden = false;
    el.textContent = `${pendingFiles.length} file(s) selected`;
  } else {
    el.hidden = true;
    el.textContent = "";
  }
}

async function handleFilesPicked(files) {
  if (!files.length) return;
  try {
    pendingFiles = await readFilesAsText(files);
  } catch (err) {
    toast(err.message || String(err));
    return;
  }
  updateFileCount();
  // Mirror the Android flow: if no credentials yet, ask for them first.
  if (!$("#import-username").value.trim() && !$("#import-password").value) {
    $("#dialog-username").value = "";
    $("#dialog-password").value = "";
    const dialog = $("#cred-dialog");
    if (typeof dialog.showModal === "function") {
      dialog.showModal();
      $("#dialog-username").focus();
    } else {
      toast("Enter a username and password, then press Import.");
    }
  }
}

// ---------------------------------------------------------------- settings
function renderSettings() {
  $("#wait-value").textContent = String(settings.waitSeconds);
  $("#healthy-value").textContent = String(settings.healthyServers);
  $("#test-without-ping").checked = settings.testWithoutPing;
}

function bindSteppers() {
  $$(".stepper-btn").forEach((btn) => {
    btn.addEventListener("click", () => {
      const which = btn.dataset.stepper;
      const dir = Number(btn.dataset.dir) || 0;
      if (which === "wait") {
        settings.waitSeconds = Math.min(60, Math.max(1, settings.waitSeconds + dir));
        localStorage.setItem("minab.waitSeconds", String(settings.waitSeconds));
      } else if (which === "healthy") {
        settings.healthyServers = Math.min(20, Math.max(1, settings.healthyServers + dir));
        localStorage.setItem("minab.healthyServers", String(settings.healthyServers));
      }
      renderSettings();
    });
  });
  $("#test-without-ping").addEventListener("change", (e) => {
    settings.testWithoutPing = e.target.checked;
    localStorage.setItem("minab.testWithoutPing", String(settings.testWithoutPing));
  });
}

// ---------------------------------------------------------------- init
function init() {
  // Tabs
  $$(".tab-btn").forEach((btn) => btn.addEventListener("click", () => switchTab(btn.dataset.tab)));

  // Connect / disconnect (big round button)
  $("#connect-btn").addEventListener("click", async () => {
    const state = String(status.state || "").toLowerCase();
    if (state === "connected" || state === "connecting") {
      await disconnect();
      return;
    }
    const target = selectedProfileId || profileById(status.profileId)?.id || profiles.find((p) => p.favorite)?.id;
    if (!target) {
      toast("Pick a server from the list first.");
      return;
    }
    await connectTo(target);
  });

  // Ping all (both Home and Configs buttons do the same thing)
  $("#ping-all-btn").addEventListener("click", () => startPing(null));
  $("#ping-all-btn-2").addEventListener("click", () => startPing(null));

  // Graph period switch + "Now" (jump to now = refresh current totals)
  $$(".period-btn").forEach((btn) =>
    btn.addEventListener("click", () => {
      graphPeriod = btn.dataset.period;
      $$(".period-btn").forEach((b) => b.classList.toggle("period-active", b === btn));
      renderGraph();
    })
  );
  $("#graph-now-btn").addEventListener("click", () => refreshUsage());

  // Import
  $("#ovpn-files").addEventListener("change", (e) => handleFilesPicked(e.target.files));
  $("#import-btn").addEventListener("click", async () => {
    if (!pendingFiles.length) {
      toast("Choose at least one .ovpn file first.");
      return;
    }
    if (!$("#import-username").value.trim() && !$("#import-password").value) {
      // No credentials yet — ask for them first (Android flow), then import.
      $("#dialog-username").value = "";
      $("#dialog-password").value = "";
      const dialog = $("#cred-dialog");
      if (typeof dialog.showModal === "function") dialog.showModal();
      return;
    }
    await doImport();
  });
  $("#cred-cancel").addEventListener("click", () => $("#cred-dialog").close());
  let credConfirmed = false;
  $("#cred-dialog form").addEventListener("submit", () => {
    credConfirmed = true;
  });
  $("#cred-dialog").addEventListener("close", () => {
    // Dialog closed via Confirm (submit) — copy credentials and import.
    if (credConfirmed) {
      credConfirmed = false;
      $("#import-username").value = $("#dialog-username").value.trim();
      $("#import-password").value = $("#dialog-password").value;
      doImport();
    }
  });

  // Real-test settings
  renderSettings();
  bindSteppers();
  $("#real-test-btn").addEventListener("click", () => {
    // NOTE: the backend contract has no real-test command yet, so there is
    // nothing to invoke here. The settings above are kept ready for it.
    toast("Real test is not wired to a backend command yet — settings are saved for the next pass.");
  });

  // Backend events
  listen("vpn-state", onVpnState);
  listen("ping-result", onPingResult);
  listen("bytecount", onBytecount);

  // First load + live status polling
  updateStatusUI(status);
  renderGraph();
  loadProfiles();
  refreshStatus();
  setInterval(refreshStatus, 2000);
}

init();
