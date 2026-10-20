// @ts-check
// The Log window, as LogView.swift: newest first, each entry expands to its command output.

/** @typedef {{date: number, title: string, output: string, success: boolean}} Entry */

/** The log as last drawn, so unrelated snapshots (every row change during a check) don't redraw it. */
let drawn = "";

/** @param {Entry[]} log */
function render(log) {
  const json = JSON.stringify(log);
  if (json === drawn) return;
  drawn = json;
  const list = $("entries");
  // Keep what's expanded and where the list is scrolled.
  const open = new Set([...list.querySelectorAll("details[open]")].map((d) => /** @type {HTMLElement} */ (d).dataset.key));
  const scroll = list.scrollTop;
  list.replaceChildren(
    ...log.map((entry) => {
      const key = `${entry.date}|${entry.title}`;
      const details = /** @type {HTMLDetailsElement} */ (el("details", "entry"));
      details.dataset.key = key;
      details.open = open.has(key);
      const summary = el("summary");
      const time = new Date(entry.date * 1000).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
      summary.append(
        icon("chevron-right", "chevron secondary"),
        icon(entry.success ? "circle-check" : "octagon-x", entry.success ? "success" : "error"),
        el("span", "entry-title", entry.title),
        el("span", "caption secondary", time),
      );
      details.append(summary, el("pre", "output", entry.output || "(no output)"));
      return details;
    }),
  );
  list.scrollTop = scroll;
  $("empty").hidden = log.length > 0;
  list.hidden = log.length === 0;
  /** @type {HTMLButtonElement} */ ($("clear")).disabled = log.length === 0;
}

$("open-file").onclick = () => invoke("open_log");
$("clear").onclick = () => invoke("clear_log");

let rendered = false;
tauri.event.listen("snapshot", (/** @type {{payload: {log: Entry[]}}} */ e) => {
  rendered = true;
  render(e.payload.log);
});
invoke("snapshot").then((/** @type {{log: Entry[]}} */ s) => s && !rendered && render(s.log));
