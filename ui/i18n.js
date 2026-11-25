'use strict';

// The window's texts in the user's language. Every text the window shows comes from
// `ui/i18n/<language>.json` through the `strings` command (src/i18n.rs): `t('key')`, with
// `{name}` placeholders from `vars`, and plural forms (`one`, `few`, `many`, `other`) picked for
// `vars.count` by the language's rules. A text the language lacks comes from English; a key
// English lacks shows as the key itself, so a mistake is visible, never blank.

let LANGUAGE = 'en';
let STRINGS = {};
let FALLBACK = null;
let PLURALS = new Intl.PluralRules('en');

/** Reads the texts for the language in use. Call it before the first render, and after the language setting changes. */
async function loadStrings() {
  const { language, strings, fallback } = await window.__TAURI__.core.invoke('strings');
  LANGUAGE = language;
  STRINGS = strings;
  FALLBACK = fallback;
  PLURALS = new Intl.PluralRules(language);
  document.documentElement.lang = language;
}

function lookup(root, key) {
  return key.split('.').reduce((v, part) => (v && typeof v === 'object' ? v[part] : undefined), root);
}

/** The text for `key`, with its placeholders filled in. */
function t(key, vars = {}) {
  let value = lookup(STRINGS, key);
  if (value === undefined && FALLBACK) value = lookup(FALLBACK, key);
  if (value && typeof value === 'object') {
    const form = PLURALS.select(Number(vars.count ?? 0));
    value = value[form] ?? value.other ?? Object.values(value)[0];
  }
  if (typeof value !== 'string') return key;
  return value.replace(/\{(\w+)\}/g, (match, name) => (name in vars ? String(vars[name]) : match));
}

/** The language in use, like `en`. */
function language() {
  return LANGUAGE;
}

/**
 * Fills the page's own texts: an element with `data-t="key"` gets the text as its content, and
 * `data-t-title`, `data-t-aria-label`, `data-t-placeholder` set those attributes.
 */
function translatePage(root = document) {
  for (const node of root.querySelectorAll('[data-t]')) node.textContent = t(node.dataset.t);
  for (const attr of ['title', 'aria-label', 'placeholder']) {
    for (const node of root.querySelectorAll(`[data-t-${attr}]`)) node.setAttribute(attr, t(node.getAttribute(`data-t-${attr}`)));
  }
}
