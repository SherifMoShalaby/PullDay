// @ts-check
// Settings: General, Sources and Items, as SettingsView.swift. Every control saves on `change` (a text field also
// 400 ms after typing stops), and a re-render never replaces or overwrites the field that has focus.

/**
 * @typedef {{id: string, name: string, symbol: string, meta: string, ignored: boolean, postCommand: string | null}} Item
 * @typedef {{title: string, roots: string, found: boolean}} AgentInfo
 * @typedef {{id: string, title: string, explanation: string, symbol: string, enabled: boolean}} Kind
 * @typedef {{checkInterval: number, autoUpdate: boolean, notify: boolean, launchAtLogin: boolean}} SettingsPrefs
 * @typedef {{items: Item[], agents: AgentInfo[], kinds: Kind[], prefs: SettingsPrefs}} Snapshot
 */

const TABS = ["general", "sources", "items"];

/** @param {string} id */
const input = (id) => /** @type {HTMLInputElement} */ ($(id));

function showTab() {
  const tab = TABS.includes(location.hash.slice(1)) ? location.hash.slice(1) : "general";
  for (const t of TABS) {
    $(t).hidden = t !== tab;
    $(`tab-${t}`).setAttribute("aria-selected", String(t === tab));
  }
  invoke("remember_tab", { tab });
}
window.addEventListener("hashchange", showTab);
// The panel's Post-Update Command… on an open window (show_window).
tauri.event.listen("tab", (/** @type {{payload: string}} */ e) => (location.hash = e.payload));

/**
 * Values just sent, by control ("checkInterval", "kind:uv", "ignore:<id>", "cmd:<id>"). A change queues behind a
 * running check, whose progress snapshots still carry the old prefs, so a control shows what was sent until a
 * snapshot agrees (or the change is refused).
 * @type {Map<string, unknown>}
 */
const pending = new Map();

/** @type {Snapshot | null} */
let last = null;

/**
 * What a control shows: the pending value until the snapshot's `actual` matches it.
 * @template T @param {string} key @param {T} actual @param {(v: any) => any} [norm]
 * @returns {T}
 */
function shown(key, actual, norm = (v) => v) {
  if (!pending.has(key)) return actual;
  if (norm(pending.get(key)) === norm(actual)) {
    pending.delete(key);
    return actual;
  }
  return /** @type {T} */ (pending.get(key));
}

/** @param {string} cmd @param {Record<string, unknown>} args @param {string} key @param {unknown} value */
function send(cmd, args, key, value) {
  pending.set(key, value);
  tauri.core.invoke(cmd, args).catch((/** @type {unknown} */ e) => {
    console.error(e);
    pending.delete(key);
    if (last) render(last);
  });
}

/** @param {string} key @param {unknown} value */
const save = (key, value) => send("set_setting", { key, value }, key, value);

// General

const interval = /** @type {HTMLSelectElement} */ ($("interval"));
interval.onchange = () => save("checkInterval", Number(interval.value));
input("auto-update").onchange = () => save("autoUpdate", input("auto-update").checked);
input("notify").onchange = () => save("notify", input("notify").checked);
input("login").onchange = () => {
  $("login-error").hidden = true;
  tauri.core.invoke("set_launch_at_login", { on: input("login").checked }).catch((/** @type {string} */ e) => {
    $("login-error").textContent = String(e);
    $("login-error").hidden = false;
  });
};

/** @param {SettingsPrefs} prefs */
function renderGeneral(prefs) {
  const seconds = shown("checkInterval", prefs.checkInterval);
  if (document.activeElement !== interval) interval.value = String(seconds);
  input("auto-update").checked = shown("autoUpdate", prefs.autoUpdate);
  input("auto-update").disabled = seconds === 0;
  input("notify").checked = shown("notify", prefs.notify);
  input("login").checked = prefs.launchAtLogin;
}

// Sources

/** @param {AgentInfo[]} agents */
function renderAgents(agents) {
  $("agents").replaceChildren(
    ...agents.map((a) => {
      const row = el("div", "field");
      const texts = el("div", "texts");
      texts.append(el("div", "", a.title), el("div", "caption secondary", a.roots));
      row.append(texts, el("span", `caption ${a.found ? "success" : "secondary"}`, a.found ? "Found" : "Not found"));
      return row;
    }),
  );
}

/** @type {Map<string, HTMLInputElement>} */
const kindToggles = new Map();

/** @param {Kind[]} kinds */
function renderKinds(kinds) {
  if (kindToggles.size !== kinds.length) {
    kindToggles.clear();
    $("kinds").replaceChildren(
      ...kinds.map((k) => {
        const row = /** @type {HTMLLabelElement} */ (el("label", "field kind-field"));
        const texts = el("div", "texts");
        const title = el("div", "kind-title");
        title.append(icon(SYMBOLS[/** @type {keyof typeof SYMBOLS} */ (k.symbol)] || "box", "secondary"), el("span", "", k.title));
        texts.append(title, withCode(el("div", "caption secondary"), k.explanation));
        const toggle = /** @type {HTMLInputElement} */ (el("input"));
        toggle.type = "checkbox";
        toggle.setAttribute("role", "switch");
        toggle.dataset.kind = k.id;
        // One kind on or off, never the whole set: other toggles may still show values not saved yet.
        toggle.onchange = () => {
          send("set_setting", { key: "enabledKinds", value: { id: k.id, on: toggle.checked } }, `kind:${k.id}`, toggle.checked);
        };
        kindToggles.set(k.id, toggle);
        row.append(texts, toggle);
        return row;
      }),
    );
  }
  for (const k of kinds) {
    const toggle = kindToggles.get(k.id);
    if (toggle) toggle.checked = shown(`kind:${k.id}`, k.enabled);
  }
}

// Items

/** @type {Map<string, {row: HTMLElement, ignore: HTMLInputElement, command: HTMLInputElement}>} */
const itemRows = new Map();

/** @param {Item} item */
function itemRow(item) {
  const row = el("div", "item");
  const head = el("div", "item-head");
  const ignoreLabel = el("label", "check");
  const ignore = /** @type {HTMLInputElement} */ (el("input"));
  ignore.type = "checkbox";
  ignore.onchange = () => send("set_ignored", { id: item.id, on: ignore.checked }, `ignore:${item.id}`, ignore.checked);
  ignoreLabel.append(ignore, document.createTextNode("Ignore"));
  head.append(
    icon(SYMBOLS[/** @type {keyof typeof SYMBOLS} */ (item.symbol)] || "box", "secondary"),
    el("span", "name", item.name),
    el("span", "caption secondary meta", item.meta),
    el("span", "spacer"),
    ignoreLabel,
  );
  const command = /** @type {HTMLInputElement} */ (el("input", "command"));
  command.type = "text";
  command.placeholder = "Post-update command (optional)";
  command.spellcheck = false;
  command.setAttribute("aria-label", `Post-update command for ${item.name}`);
  // Saved 400 ms after typing stops too, so closing the window after typing keeps the command.
  // shortcut: a close within those 400 ms loses the last keystrokes (pagehide can't send them); flush on CloseRequested if that bites.
  /** @type {number | undefined} */
  let timer;
  const saveCommand = () => {
    clearTimeout(timer);
    send("set_setting", { key: "postCommand", value: { id: item.id, command: command.value } }, `cmd:${item.id}`, command.value);
  };
  command.oninput = () => {
    pending.set(`cmd:${item.id}`, command.value);
    clearTimeout(timer);
    timer = setTimeout(saveCommand, 400);
  };
  command.onchange = saveCommand;
  row.append(head, command);
  return { row, ignore, command };
}

/** @param {Item[]} items */
function renderItems(items) {
  $("items-empty").hidden = items.length > 0;
  const rows = items.map((item) => {
    let r = itemRows.get(item.id);
    if (!r) itemRows.set(item.id, (r = itemRow(item)));
    r.ignore.checked = shown(`ignore:${item.id}`, item.ignored);
    // Never overwrite what's being typed: a check finishing mid-edit re-renders. A blank command is saved as none.
    const command = shown(`cmd:${item.id}`, item.postCommand ?? "", (v) => (String(v).trim() ? v : ""));
    if (document.activeElement !== r.command) r.command.value = command;
    return r.row;
  });
  for (const id of [...itemRows.keys()]) if (!items.some((i) => i.id === id)) itemRows.delete(id);
  const list = $("item-list");
  // Only reorder when the list changed; then put the focus and selection back where they were.
  if (rows.length !== list.children.length || rows.some((r, i) => list.children[i] !== r)) {
    const focused = /** @type {HTMLInputElement | null} */ (list.contains(document.activeElement) ? document.activeElement : null);
    const [start, end] = focused?.type === "text" ? [focused.selectionStart, focused.selectionEnd] : [null, null];
    list.replaceChildren(...rows);
    if (focused?.isConnected) {
      focused.focus();
      if (start !== null) focused.setSelectionRange(start, end);
    }
  }
}

/** @param {Snapshot} snap */
function render(snap) {
  last = snap;
  renderGeneral(snap.prefs);
  renderAgents(snap.agents);
  renderKinds(snap.kinds);
  renderItems(snap.items);
}

withCode($("items-intro"), $("items-intro").textContent || "");
showTab();
let rendered = false;
tauri.event.listen("snapshot", (/** @type {{payload: Snapshot}} */ e) => {
  rendered = true;
  render(e.payload);
});
invoke("snapshot").then((/** @type {Snapshot} */ s) => s && !rendered && render(s));
