// @ts-check
// Shared by the panel, Settings and Log pages (loaded first). Text from items, repos and commands goes in with
// textContent only, never as HTML.

/** @type {any} */
const tauri = /** @type {any} */ (window).__TAURI__;
/** @param {string} cmd @param {Record<string, unknown>} [args] */
const invoke = (cmd, args) => tauri.core.invoke(cmd, args).catch(console.error);

/** SF Symbols names from the core → the vendored Lucide icons. */
const SYMBOLS = {
  "puzzlepiece.extension": "puzzle",
  "arrow.triangle.branch": "git-branch",
  shippingbox: "package",
  "cube.box": "box",
  cube: "boxes",
  terminal: "terminal",
};

/** @param {string} id */
const $ = (id) => /** @type {HTMLElement} */ (document.getElementById(id));

/**
 * @param {string} tag
 * @param {string} [cls]
 * @param {string} [text]
 */
function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

/** @param {string} name @param {string} [cls] */
const icon = (name, cls = "") => el("span", `icon i-${name} ${cls}`);

/**
 * `text` with each `backtick span` as a <code> element, as SwiftUI renders the Mac's explanations.
 * @param {HTMLElement} parent @param {string} text
 */
function withCode(parent, text) {
  parent.replaceChildren(...text.split("`").map((part, i) => (i % 2 ? el("code", "", part) : document.createTextNode(part))));
  return parent;
}
