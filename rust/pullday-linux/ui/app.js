// @ts-check
// The panel: renders the worker's snapshot (see view.rs) and sends item ids back (helpers in common.js).

/**
 * @typedef {{id: string, name: string, symbol: string, subtitle: string, tooltip: string,
 *   tone: string, trailing: string, canSkip: boolean, canStopSkipping: boolean, hasPath: boolean}} Row
 * @typedef {{id: string, title: string, rows: Row[]}} Group
 * @typedef {{id: string, title: string, pending: string[], groups: Group[]}} Section
 * @typedef {{phase: string, lastRun: number | null, updateAllIds: string[], sections: Section[],
 *   trayNote: string | null, appUpdate: string | null}} Snapshot
 */

/** Row trailing → icon, color and tooltip, as ItemRow's `trailing`. */
const TRAILING = {
  ok: ["circle-check", "success", "Up to date"],
  failed: ["triangle-alert", "warning", ""],
  skipped: ["skip-forward", "secondary", "You skipped this update. PullDay will offer the next one."],
  pinned: ["pin", "secondary", "Installed with an exact version pin, so PullDay won't update it"],
  unknown: ["circle-question-mark", "secondary", "Couldn't tell if there's an update"],
};

/** @type {Snapshot | null} */
let snap = null;

const busy = () => !snap || snap.phase !== "idle";

/** "Checked 5 minutes ago", as the Mac's relative date. */
function statusLine() {
  if (!snap) return "";
  if (snap.phase === "scanning") return "Checking for updates…";
  if (snap.phase === "updating") return "Updating…";
  if (snap.lastRun == null) return "Not checked yet";
  const seconds = Math.max(0, Math.floor(Date.now() / 1000 - snap.lastRun));
  for (const [unit, size] of [["day", 86400], ["hour", 3600], ["minute", 60], ["second", 1]]) {
    const n = Math.floor(seconds / /** @type {number} */ (size));
    if (n >= 1 || unit === "second") return `Checked ${n} ${unit}${n === 1 ? "" : "s"} ago`;
  }
  return "";
}

function render() {
  if (!snap) return;
  $("status").textContent = statusLine();
  $("busy").hidden = !busy();
  /** @type {HTMLButtonElement} */ ($("refresh")).disabled = busy();

  $("note").hidden = !snap.trayNote;
  $("note").textContent = snap.trayNote || "";

  const count = snap.updateAllIds.length;
  $("banner").hidden = count === 0;
  $("banner-text").textContent = `${count} update${count === 1 ? "" : "s"} available`;
  /** @type {HTMLButtonElement} */ ($("update-all")).disabled = busy();

  $("app-update").hidden = !snap.appUpdate;
  $("app-update-text").textContent = `PullDay ${snap.appUpdate || ""} is available`;

  const list = $("list");
  const scroll = list.scrollTop;
  list.replaceChildren();
  for (const section of snap.sections) {
    const header = el("div", "section");
    header.append(el("span", "section-title", section.title), el("span", "spacer"));
    if (section.pending.length) {
      const b = el("button", "mini", `Update all (${section.pending.length})`);
      /** @type {HTMLButtonElement} */ (b).disabled = busy();
      b.onclick = () => invoke("update", { ids: section.pending });
      header.append(b);
    }
    list.append(header);
    for (const group of section.groups) {
      list.append(el("div", "kind", group.title.toUpperCase()));
      for (const row of group.rows) list.append(rowElement(row));
    }
  }
  list.scrollTop = scroll;
  list.hidden = snap.sections.length === 0;
  renderEmpty();
}

function renderEmpty() {
  const empty = $("empty");
  empty.hidden = !snap || snap.sections.length > 0;
  if (empty.hidden) return;
  if (snap?.phase === "scanning") {
    empty.replaceChildren(icon("loader-circle", "spin"), el("div", "secondary", "Looking for skills and tools…"));
  } else {
    empty.replaceChildren(
      icon("inbox", "big secondary"),
      el("div", "empty-title", "Nothing to keep up to date yet"),
      el("div", "caption secondary",
        "PullDay looks in your agents' plugin, extension and skills folders, and your uv and pipx tools. Turn sources on or off in Settings."),
    );
  }
}

/** @param {Row} row */
function rowElement(row) {
  const r = el("div", "row");
  const texts = el("div", "texts");
  const subtitle = el("div", `subtitle ${row.tone}`, row.subtitle);
  subtitle.title = row.tooltip;
  texts.append(el("div", "name", row.name), subtitle);
  r.append(icon(SYMBOLS[/** @type {keyof typeof SYMBOLS} */ (row.symbol)] || "box", "kind-icon secondary"), texts);

  if (row.trailing === "spinner") {
    r.append(icon("loader-circle", "spin secondary"));
  } else if (row.trailing === "update") {
    const b = el("button", "small", "Update");
    /** @type {HTMLButtonElement} */ (b).disabled = busy();
    b.onclick = () => invoke("update", { ids: [row.id] });
    r.append(b);
  } else {
    const [name, tone, help] = TRAILING[/** @type {keyof typeof TRAILING} */ (row.trailing)] || TRAILING.unknown;
    const i = icon(name, tone);
    if (help) i.title = help;
    r.append(i);
  }

  const more = el("button", "plain more");
  more.setAttribute("aria-label", `Actions for ${row.name}`);
  more.append(icon("ellipsis"));
  more.onclick = (e) => {
    e.stopPropagation();
    const box = more.getBoundingClientRect();
    openMenu(row, box.right, box.bottom, true);
  };
  r.append(more);
  r.oncontextmenu = (e) => {
    e.preventDefault();
    openMenu(row, e.clientX, e.clientY);
  };
  return r;
}

/**
 * The ⋯ menu, as ItemRow's `actions`: at the cursor for a right click, ending at the button's right edge for ⋯.
 * @param {Row} row @param {number} x @param {number} y @param {boolean} [leftOfX]
 */
function openMenu(row, x, y, leftOfX = false) {
  const menu = $("menu");
  /** @type {[string, () => void, boolean?][]} */
  const entries = [["Update Now", () => invoke("update", { ids: [row.id] }), busy()]];
  if (row.canSkip) entries.push(["Skip This Update", () => invoke("skip_update", { id: row.id })]);
  if (row.canStopSkipping) entries.push(["Stop Skipping", () => invoke("stop_skipping", { id: row.id })]);
  if (row.hasPath) entries.push(["Show in Files", () => invoke("open_item", { id: row.id })]);
  entries.push(
    ["Post-Update Command…", () => invoke("show_window", { name: "settings", tab: "items" })],
    ["Show Log", () => invoke("show_window", { name: "log" })],
    ["", () => {}],
    ["Ignore", () => invoke("set_ignored", { id: row.id, on: true })],
  );
  menu.replaceChildren();
  for (const [label, action, disabled] of entries) {
    if (!label) {
      menu.append(el("div", "separator"));
      continue;
    }
    const item = /** @type {HTMLButtonElement} */ (el("button", "menu-item", label));
    item.setAttribute("role", "menuitem");
    item.disabled = !!disabled;
    item.onclick = () => {
      closeMenu();
      action();
    };
    menu.append(item);
  }
  menu.hidden = false;
  const w = menu.offsetWidth, h = menu.offsetHeight;
  menu.style.left = `${Math.max(4, Math.min(leftOfX ? x - w : x, window.innerWidth - w - 4))}px`;
  menu.style.top = `${Math.max(4, y + h > window.innerHeight - 4 ? y - h : y)}px`;
  /** @type {HTMLElement | null} */ (menu.querySelector("button:not(:disabled)"))?.focus();
}

function closeMenu() {
  $("menu").hidden = true;
}

document.addEventListener("click", (e) => {
  if (!$("menu").contains(/** @type {Node} */ (e.target))) closeMenu();
});
// Hiding the panel (focus loss, Escape) blurs it.
window.addEventListener("blur", closeMenu);
document.addEventListener("keydown", (e) => {
  if (e.key !== "Escape") return;
  if (!$("menu").hidden) closeMenu();
  else invoke("hide_panel");
});

$("refresh").onclick = () => invoke("check_now");
$("update-all").onclick = () => invoke("update_all");
$("download").onclick = () => invoke("open_release");
$("settings").onclick = () => invoke("show_window", { name: "settings" });
$("log").onclick = () => invoke("show_window", { name: "log" });
$("quit").onclick = () => invoke("quit");

// The window follows the content's height (the list itself stops at 440 px and scrolls, as on the Mac).
let reported = 0;
new ResizeObserver(() => {
  const px = Math.ceil($("app").getBoundingClientRect().height);
  if (px !== reported) {
    reported = px;
    invoke("panel_height", { px });
  }
}).observe($("app"));

tauri.event.listen("snapshot", (/** @type {{payload: Snapshot}} */ e) => {
  snap = e.payload;
  render();
});
invoke("snapshot").then((/** @type {Snapshot} */ s) => {
  snap = snap || s;
  render();
});
setInterval(() => ($("status").textContent = statusLine()), 30000);
