'use strict';

// steambro's window. Talks to the Rust side through commands (src/app/commands.rs) and loads
// artwork through the `art` URL scheme (src/app/protocol.rs). The sidebar lists the games by
// status; the content shows one status's games as tiles, a game's page, or the settings.

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const ART = 'http://art.localhost';
const SVG = 'http://www.w3.org/2000/svg';

const STATUS = {
  steambro: 'In Steam',
  user: 'In Steam',
  none: 'Not in Steam',
};

// The sidebar's sections, the ones that need something first. A game is in the first section
// it matches; a section without games doesn't show.
const SECTIONS = [
  { id: 'none', label: 'Not in Steam', hint: 'Installed on this PC, not in Steam yet.', test: (g) => !g.gone && g.status === 'none' },
  { id: 'old', label: 'Old shortcuts', hint: 'steambro added these, and their shortcuts start the game an older way.', test: (g) => g.outdated },
  { id: 'gone', label: 'Uninstalled', hint: 'Still in Steam, but gone from this PC.', test: (g) => g.gone },
  { id: 'in', label: 'In Steam', hint: 'steambro added these to Steam.', test: (g) => g.status === 'steambro' },
  { id: 'user', label: 'Added elsewhere', hint: 'You or another tool added these to Steam. steambro leaves their shortcuts alone.', test: (g) => g.status === 'user' },
];
const ALL = { id: 'all', label: 'Library', hint: 'All your games, whatever their status.' };

// The order of the store tags; a store steambro doesn't know comes last.
const STORE_ORDER = ['xbox', 'battlenet', 'epic', 'gog', 'exe'];
// The stores the empty library names, until the settings view has the real list.
const KNOWN_STORES = [{ id: 'xbox', name: 'Xbox' }, { id: 'battlenet', name: 'Battle.net' }, { id: 'epic', name: 'Epic Games' }, { id: 'gog', name: 'GOG' }];

const ICONS = {
  fold: 'M9 6l6 6-6 6',
  tiles: 'M4 4h6v6H4zM14 4h6v6h-6zM4 14h6v6H4zM14 14h6v6h-6z',
  plus: 'M12 5v14M5 12h14',
  refresh: 'M20 12a8 8 0 1 1-2.34-5.66M20 4v5h-5',
  // A window, for an added exe or a store without a logo.
  window: 'M4 5h16a1 1 0 0 1 1 1v12a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1zM3 9h18',
};

let view = null;
// The games found, then the Steam shortcuts of games gone from the PC (`goneGame`).
let everything = [];
// What the content shows: 'tiles' for a section's games, 'game' for a game's page, or 'settings'.
let mode = 'tiles';
// The section the tiles show, or ALL; the game page goes back to it.
let openSection = ALL.id;
let openKey = null;
// The game page's tab, kept from game to game.
let detailTab = 'info';
// Where the settings go back to.
let beforeSettings = 'tiles';
// The folded sections of the sidebar, by ID, remembered for the next start.
const folded = new Set(remembered('folded', []));
// The tiles: the search text, the store tags turned on, and the selected games, by key.
let query = '';
const tags = new Set();
const selected = new Set();
// The game a shift-click selects from.
let anchorKey = null;
// The tiles' cards, by game key.
let cards = new Map();
let busy = false;
let loadFailed = false;
// Added to image URLs so edited artwork loads again.
let artVersion = Date.now();

const $ = (selector) => document.querySelector(selector);

// `h` (creating elements) and `choose` (the choice dialog) are in choice.js; the store logos in
// logos.js.

/** A value this window stored before, or `fallback`. Storage can be missing or refuse. */
function remembered(name, fallback) {
  try {
    return JSON.parse(localStorage.getItem(`steambro.${name}`)) ?? fallback;
  } catch {
    return fallback;
  }
}

function remember(name, value) {
  try {
    localStorage.setItem(`steambro.${name}`, JSON.stringify(value));
  } catch {
    // Only a convenience.
  }
}

// ---------------------------------------------------------------- data

async function load(refresh = false) {
  try {
    view = await invoke('library', { refresh });
    loadFailed = false;
  } catch (error) {
    loadFailed = true;
    toast(`steambro couldn't read the library: ${error}`, 'error');
    return;
  }
  everything = [...view.games, ...view.gone.map(goneGame)];
  render();
}

/** The Steam shortcut of a game gone from the PC, as a game with Steam's own artwork. */
function goneGame(g) {
  const steams = (slot, label, shape) => ({ slot, label, shape, inSteam: true, placement: 'kept' });
  return {
    ...g,
    source: g.source || 'Unknown store',
    gone: true,
    status: 'steambro',
    outdated: false,
    isApp: false,
    slots: [steams('cover', 'cover', [600, 900]), steams('hero', 'header', [3840, 1240]), steams('logo', 'logo', null), steams('wide', 'wide cover', [920, 430]), steams('icon', 'icon', null)],
  };
}

function game(key) {
  return everything.find((g) => g.key === key);
}

function sectionOf(g) {
  return SECTIONS.find((s) => s.test(g));
}

function section(id) {
  return SECTIONS.find((s) => s.id === id) || ALL;
}

function sectionGames(id) {
  return id === ALL.id ? everything : everything.filter((g) => sectionOf(g).id === id);
}

// ---------------------------------------------------------------- artwork frames

/**
 * The default crop: the largest area with the slot's shape, as fractions of the image, centered
 * or kept at the top (`anchor`). Matches `Crop::default_for` in src/art/mod.rs.
 */
function defaultCrop(width, height, shape, anchor) {
  const imageRatio = width / height;
  const ratio = shape[0] / shape[1];
  if (imageRatio > ratio) {
    const w = ratio / imageRatio;
    return { x: (1 - w) / 2, y: 0, w, h: 1 };
  }
  const h = imageRatio / ratio;
  return { x: 0, y: anchor === 'top' ? 0 : (1 - h) / 2, w: 1, h };
}

/** Scales and moves `img` inside its frame so the frame shows exactly the crop. */
function showCrop(img, crop) {
  img.style.width = `${100 / crop.w}%`;
  img.style.height = `${100 / crop.h}%`;
  img.style.left = `${(-crop.x / crop.w) * 100}%`;
  img.style.top = `${(-crop.y / crop.h) * 100}%`;
}

function slotOf(g, slot) {
  return g.slots.find((s) => s.slot === slot);
}

/** What the slot shows: Steam's own image where steambro left Steam's alone, else steambro's source. */
function slotImage(g, s) {
  const steams = s.inSteam && (s.placement === 'kept' || g.status === 'user');
  if (steams) return { url: `${ART}/steam/${s.slot}/${g.appId}?v=${artVersion}`, cropped: false };
  return { url: `${ART}/source/${s.slot}/${encodeURIComponent(g.key)}?v=${artVersion}`, cropped: Boolean(s.shape) };
}

/**
 * A box with the slot's shape showing its image. `onMissing` runs when there is no image;
 * `fallback` is shown instead.
 */
function frame(g, slot, { fallback, onMissing, crop } = {}) {
  const s = slotOf(g, slot);
  const shape = s.shape || [1, 1];
  const box = h('div.frame.loading');
  box.style.setProperty('--shape', `${shape[0]} / ${shape[1]}`);
  if (!s.shape) box.classList.add('checker');
  const { url, cropped } = slotImage(g, s);
  const img = h('img', { alt: '', decoding: 'async', draggable: 'false' });
  img.onload = () => {
    box.classList.remove('loading');
    if (cropped) showCrop(img, crop || s.crop || defaultCrop(img.naturalWidth, img.naturalHeight, s.shape, s.anchor));
    else img.className = s.shape ? 'fill' : 'whole';
  };
  img.onerror = () => {
    box.classList.remove('loading');
    img.remove();
    if (fallback) box.append(fallback);
    if (onMissing) onMissing();
  };
  img.src = url;
  box.append(img);
  return box;
}

// ---------------------------------------------------------------- icons

/** A filled SVG path in a 24 by 24 box, like a store's logo; `outline` draws it as a line. */
function pathIcon(d, cls, outline = false) {
  const svg = document.createElementNS(SVG, 'svg');
  svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('aria-hidden', 'true');
  svg.setAttribute('class', `${outline ? 'line-icon' : 'fill-icon'} ${cls || ''}`.trim());
  const p = document.createElementNS(SVG, 'path');
  p.setAttribute('d', d);
  svg.append(p);
  return svg;
}

/** A store's logo, in its color; an added exe, or a store without a logo, gets a window. */
function storeLogo(store, cls = 'store-logo') {
  const color = `store-${STORE_LOGOS[store] ? store : 'other'}`;
  return STORE_LOGOS[store] ? pathIcon(STORE_LOGOS[store], `${cls} ${color}`) : pathIcon(ICONS.window, `${cls} ${color}`, true);
}

function svgIcon(path, cls) {
  return pathIcon(path, cls, true);
}

// ---------------------------------------------------------------- rendering

function render() {
  if (!view) return;
  showMood();
  renderAccount();
  renderSidebar();
  renderContent();
  renderNotices();
}

/** The Steam account at the bottom of the sidebar. */
function renderAccount() {
  const account = view.steam.account;
  const name = account ? account.name || `account ${account.id}` : view.steam.installed ? 'No Steam account yet' : 'Steam not found';
  $('#account').replaceChildren(pathIcon(STEAM_LOGO, 'steam-logo'), h('span.account-name', {}, name), account && h(`span.dot${view.steam.running ? '.on' : ''}`));
  $('#account').title = account ? `Steam account ${name}. Steam is ${view.steam.running ? 'running' : 'closed'}.` : name;
}

/** Shows what the content is set to: a section's tiles, a game's page, or the settings. */
function renderContent() {
  if (mode === 'game' && !game(openKey)) mode = 'tiles';
  $('#tiles').hidden = mode !== 'tiles';
  $('#detail').hidden = mode !== 'game';
  $('#settings').hidden = mode !== 'settings';
  if (mode === 'tiles') renderTiles();
  else if (mode === 'game') renderDetail();
  else renderSettings();
}

/** Shows a section's games as tiles, or with ALL every game. */
function openTiles(id) {
  if (id !== openSection) {
    tags.clear();
    query = '';
    $('#search').value = '';
    selected.clear();
    anchorKey = null;
  }
  openSection = id;
  mode = 'tiles';
  openKey = null;
  renderSidebar();
  renderContent();
  $('#library').scrollTop = 0;
}

/** Opens a game's page and selects its row in the sidebar, unfolding its section. */
function openGame(key) {
  openKey = key;
  mode = 'game';
  const g = game(key);
  if (g && folded.delete(sectionOf(g).id)) remember('folded', [...folded]);
  renderSidebar();
  renderContent();
  $('#detail').scrollTop = 0;
  $(`.game-row[data-key="${CSS.escape(key)}"]`)?.scrollIntoView({ block: 'nearest' });
}

/** Back from a game's page to the tiles it came from. */
function closeGame() {
  openTiles(openSection);
}

function openSettings() {
  if (mode !== 'settings') beforeSettings = mode;
  mode = 'settings';
  renderSidebar();
  renderContent();
  $('#settings').scrollTop = 0;
  loadSettings();
}

function closeSettings() {
  mode = beforeSettings === 'game' && game(openKey) ? 'game' : 'tiles';
  renderSidebar();
  renderContent();
}

// ---------------------------------------------------------------- sidebar

/** The sections with their games, under the brand; the account and the settings stay at the bottom. */
function renderSidebar() {
  const nav = $('#sections');
  const scroll = nav.scrollTop;
  $('#all-count').textContent = String(everything.length);
  const allOpen = mode === 'tiles' && openSection === ALL.id;
  $('#all-games').classList.toggle('open', allOpen);
  const groups = SECTIONS.map((s) => [s, sectionGames(s.id)]).filter(([, games]) => games.length);
  nav.replaceChildren(...(groups.length
    ? groups.map(([s, games]) => sectionGroup(s, games))
    : [h('p.side-empty', {}, view && !loadFailed ? 'No games yet.' : '')]));
  nav.scrollTop = scroll;
  $('#open-settings').classList.toggle('on', mode === 'settings');
}

function sectionGroup(s, games) {
  const isFolded = folded.has(s.id);
  const open = mode === 'tiles' && openSection === s.id;
  const fold = h('button.fold', {
    title: isFolded ? 'Show these games' : 'Hide these games',
    'aria-label': `${isFolded ? 'Show' : 'Hide'} the games ${s.label.toLowerCase()}`,
    'aria-expanded': String(!isFolded),
    onclick: () => {
      if (isFolded) folded.delete(s.id); else folded.add(s.id);
      remember('folded', [...folded]);
      renderSidebar();
    },
  }, svgIcon(ICONS.fold));
  const link = h(`button.section-link.tone-${s.id}${open ? '.open' : ''}`, {
    title: `Show these ${games.length} as tiles`,
    'aria-current': open ? 'page' : null,
    onclick: () => openTiles(s.id),
  }, h('span.section-label', {}, s.label), h('span.count', {}, String(games.length)), svgIcon(ICONS.tiles, 'tiles-icon'));
  return h(`div.section-group${isFolded ? '.folded' : ''}`, {},
    h('div.section-head', {}, fold, link),
    !isFolded && h('div.section-games', {}, ...games.map(gameRow)));
}

function gameRow(g) {
  const current = mode === 'game' && openKey === g.key;
  return h(`button.game-row${current ? '.current' : ''}`, {
    'data-key': g.key,
    title: `${g.name} · ${sourceLabel(g)}`,
    'aria-current': current ? 'page' : null,
    onclick: () => openGame(g.key),
    oncontextmenu: (e) => { e.preventDefault(); openCardMenu(g, e.clientX, e.clientY); },
  }, rowIcon(g), h('span.row-name', {}, g.name), storeLogo(g.store, 'row-store'));
}

/** The game's icon for its row, or its store's logo when it has none. */
function rowIcon(g) {
  const s = slotOf(g, 'icon');
  const box = h('span.row-icon');
  if (!s) return box.appendChild(storeLogo(g.store)) && box;
  const img = h('img', { alt: '', loading: 'lazy', decoding: 'async', draggable: 'false' });
  img.onerror = () => img.replaceWith(storeLogo(g.store));
  img.src = slotImage(g, s).url;
  box.append(img);
  return box;
}

// ---------------------------------------------------------------- tiles

/** The open section's title, its cards, and the toolbar. Search and tags then only show or hide cards. */
function renderTiles() {
  if (openSection !== ALL.id && !sectionGames(openSection).length) openSection = ALL.id;
  const s = section(openSection);
  const games = sectionGames(s.id);
  const nothing = !everything.length;
  $('#tiles').classList.toggle('nothing', nothing);
  $('#tiles-title').replaceChildren(...[s.label, !nothing && h('span.count', {}, String(games.length))].filter(Boolean));
  $('#tiles-hint').textContent = nothing ? '' : s.hint;
  if (nothing) {
    cards = new Map();
    selected.clear();
    $('#selection').hidden = true;
    $('#library').replaceChildren(emptyLibrary());
    return;
  }
  const library = $('#library');
  const scroll = library.scrollTop;
  for (const key of [...selected]) if (!games.some((g) => g.key === key)) selected.delete(key);
  cards = new Map(games.map((g) => [g.key, card(g)]));
  library.replaceChildren(...cards.values(), h('div.empty', { hidden: true }));
  applyFilter();
  showSelection();
  library.scrollTop = scroll;
}

/** No games at all: what steambro reads, and how to add a game it can't find. */
function emptyLibrary() {
  const stores = settingsView ? settingsView.stores.filter((st) => st.id !== 'exe') : KNOWN_STORES;
  return h('div.empty-library', {},
    h('img.empty-cloud', { src: 'logo-glance.svg', alt: '', draggable: 'false' }),
    h('h2', {}, 'No games found yet'),
    h('p', {}, 'steambro reads what these stores installed for your Windows account, on any drive:'),
    h('div.store-pills', {}, ...stores.map((st) => h('span.store-pill', {}, storeLogo(st.id), st.id === 'xbox' ? 'Xbox app and Game Pass' : st.name))),
    h('p', {}, 'A game from anywhere else, or one these stores keep no record of, goes in by its exe.'),
    h('div.empty-actions', {},
      h('button.primary', { onclick: addExe }, svgIcon(ICONS.plus), 'Add a game by its exe'),
      h('button', { onclick: rescan }, svgIcon(ICONS.refresh), 'Look again')));
}

function card(g) {
  const check = h('button.check', {
    role: 'checkbox',
    'aria-label': `Select ${g.name}`,
    title: 'Select',
    onclick: (e) => { e.stopPropagation(); toggleSelected(g, e.shiftKey); },
  });
  const node = h('div.card', {
    tabindex: '0',
    title: g.name,
    'data-key': g.key,
    // Only the checkbox in the corner selects; a click anywhere else opens the game.
    onclick: () => openGame(g.key),
    oncontextmenu: (e) => { e.preventDefault(); openCardMenu(g, e.clientX, e.clientY); },
    onkeydown: (e) => {
      if (e.target !== node) return;
      if (e.key === 'Enter') openGame(g.key);
      else if (e.key === ' ') { e.preventDefault(); toggleSelected(g, e.shiftKey); }
      else if (e.key === 'ContextMenu' || (e.shiftKey && e.key === 'F10')) {
        e.preventDefault();
        const box = node.getBoundingClientRect();
        openCardMenu(g, box.left + 24, box.top + 24);
      }
    },
  },
  h('div.cover', {},
    frame(g, 'cover', { fallback: h('div.placeholder', {}, h('div', {}, h('strong', {}, g.name), g.source)) }),
    check),
  h('div.card-name', {}, g.name),
  meta(g, true));
  return node;
}

/**
 * The store and the status of a game. On a card, the status is a dot in the color of its section;
 * the game page spells it out.
 */
function meta(g, dot = false) {
  const [text, tone] = statusOf(g);
  return h('div.card-meta', {},
    h('span.source', {}, storeLogo(g.store), h('span.source-name', {}, sourceLabel(g))),
    dot ? h(`span.status.dot.${tone}`, { title: text, 'aria-label': text }) : h(`span.status.${tone}`, {}, text));
}

/** A game's status for display: [text, class]. */
function statusOf(g) {
  if (g.gone) return ['Uninstalled', 'gone'];
  if (g.outdated) return ['Old shortcut', 'old'];
  return [STATUS[g.status], g.status];
}

// ---------------------------------------------------------------- search and tags

/** The store tags of the open section: one per store its games come from. */
function storeTags() {
  const stores = new Map(sectionGames(openSection).map((g) => [g.store, g.source]));
  const rank = (id) => (STORE_ORDER.includes(id) ? STORE_ORDER.indexOf(id) : STORE_ORDER.length);
  return [...stores].sort(([a], [b]) => rank(a) - rank(b))
    .map(([store, source]) => ({ id: store, label: source, test: (g) => g.store === store }));
}

/** The open section's games that match the search. */
function searched() {
  const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  return sectionGames(openSection).filter((g) => words.every((w) => g.name.toLowerCase().includes(w)));
}

/** The open section's games that match the search and the store tags (any of them). */
function visibleGames() {
  return searched().filter((g) => !tags.size || tags.has(g.store));
}

/** Shows the cards that match the search and the tags, and updates the toolbar. */
function applyFilter() {
  const shown = new Set(visibleGames().map((g) => g.key));
  for (const [key, node] of cards) node.hidden = !shown.has(key);
  const empty = $('#library .empty');
  if (!empty) return;
  empty.hidden = shown.size > 0;
  if (!shown.size) {
    empty.replaceChildren(
      h('strong', {}, 'Nothing here'),
      query.trim() ? `No game here matches “${query.trim()}”${tags.size ? ' in these stores' : ''}.` : 'No game here comes from these stores.');
  }
  renderToolbar();
}

function renderToolbar() {
  const stores = storeTags();
  for (const id of [...tags]) if (!stores.some((t) => t.id === id)) tags.delete(id);
  const pool = searched();
  const chip = (label, on, count, onclick, icon) =>
    h('button.tag', { 'aria-pressed': String(on), onclick }, icon, label, h('span.count', {}, String(count)));
  $('#tags').replaceChildren(
    chip('All', tags.size === 0, pool.length, () => { tags.clear(); applyFilter(); }),
    ...stores.map((t) => chip(t.label, tags.has(t.id), pool.filter(t.test).length, () => {
      if (tags.has(t.id)) tags.delete(t.id); else tags.add(t.id);
      applyFilter();
    }, storeLogo(t.id))));
  renderSelectionBar();
}

// ---------------------------------------------------------------- selection

function selectedGames() {
  return everything.filter((g) => selected.has(g.key));
}

/** Selects or unselects a game; with `range`, every shown game from the last one clicked. */
function toggleSelected(g, range = false) {
  const shown = visibleGames();
  const from = shown.findIndex((x) => x.key === anchorKey);
  const to = shown.findIndex((x) => x.key === g.key);
  if (range && from >= 0 && to >= 0) {
    for (const x of shown.slice(Math.min(from, to), Math.max(from, to) + 1)) selected.add(x.key);
  } else if (selected.has(g.key)) {
    selected.delete(g.key);
  } else {
    selected.add(g.key);
  }
  anchorKey = g.key;
  showSelection();
}

function selectAll() {
  for (const g of visibleGames()) selected.add(g.key);
  showSelection();
}

function clearSelection() {
  selected.clear();
  anchorKey = null;
  showSelection();
}

/** Marks the selected cards without rebuilding them, and updates the selection bar. */
function showSelection() {
  for (const [key, node] of cards) {
    node.classList.toggle('selected', selected.has(key));
    node.querySelector('.check').setAttribute('aria-checked', String(selected.has(key)));
  }
  $('#library').classList.toggle('selecting', selected.size > 0);
  renderSelectionBar();
}

/**
 * The bar at the bottom while games are selected: how many, Select all and None, and what the
 * selected games can do in Steam. Selecting starts at a card's corner, its menu, or Ctrl+A.
 */
function renderSelectionBar() {
  const bar = $('#selection');
  const games = selectedGames();
  bar.hidden = !games.length;
  if (!games.length) return bar.replaceChildren();
  const hidden = games.filter((g) => cards.get(g.key)?.hidden).length;
  const all = visibleGames().every((g) => selected.has(g.key));
  bar.replaceChildren(
    h('span.selected-count', {}, `${games.length} selected`, hidden ? h('small', {}, ` (${hidden} hidden by the filter)`) : null),
    h('button.ghost', { onclick: selectAll, disabled: all }, 'Select all'),
    h('button.ghost', { onclick: clearSelection }, 'None'),
    h('span.bar-gap'),
    ...steamActions(games, true).map((a) =>
      h(`button${a.primary ? '.primary' : ''}${a.danger ? '.danger' : ''}`, { onclick: a.run, disabled: busy }, a.label)));
}

/**
 * What can be done in Steam with these games: add the ones not in Steam, update outdated
 * shortcuts, remove steambro's shortcuts. `fromSelection` clears the selection after a change.
 */
function steamActions(games, fromSelection = false) {
  const one = games.length === 1;
  const run = (subset, mode) => async () => {
    if ((await changeInSteam(subset, mode)) && fromSelection) clearSelection();
  };
  const add = games.filter((g) => g.status === 'none');
  const update = games.filter((g) => g.outdated);
  const remove = games.filter((g) => g.status === 'steambro');
  const actions = [];
  if (add.length && view.steam.account) {
    actions.push({ label: one ? 'Add to Steam' : `Add ${add.length} to Steam`, primary: true, run: run(add, 'add') });
  }
  if (update.length) actions.push({ label: one ? 'Update shortcut' : `Update ${update.length}`, run: run(update, 'update') });
  if (remove.length) actions.push({ label: one ? 'Remove from Steam' : `Remove ${remove.length} from Steam`, danger: true, run: run(remove, 'remove') });
  return actions;
}

// ---------------------------------------------------------------- right-click menu

/** steambro's own menu for a game, on its card or its sidebar row, in place of the browser's. */
function openCardMenu(g, x, y) {
  const group = mode === 'tiles' && selected.has(g.key) && selected.size > 1;
  const games = group ? selectedGames() : [g];
  const items = group
    ? [{ label: `${games.length} games selected`, header: true }, ...steamActions(games, true), null, { label: 'Clear selection', run: clearSelection }]
    : [
      { label: 'Open', run: () => openGame(g.key), strong: true },
      null,
      ...steamActions(games),
      !g.gone && { label: 'Open folder', run: () => invoke('open_folder', { key: g.key }) },
      ...(mode === 'tiles' && cards.has(g.key) ? [null, { label: selected.has(g.key) ? 'Unselect' : 'Select', run: () => toggleSelected(g) }] : []),
    ].filter((item) => item !== false);
  openMenu(x, y, items);
}

/** Shows a menu at (x, y). An item is `{ label, run, danger, strong, header }`; `null` is a line. */
function openMenu(x, y, items) {
  const menu = $('#menu');
  const close = () => {
    menu.hidden = true;
    document.removeEventListener('pointerdown', outside, true);
    window.removeEventListener('blur', close);
    window.removeEventListener('resize', close);
    $('#library').removeEventListener('scroll', close);
    $('#sections').removeEventListener('scroll', close);
  };
  const outside = (e) => { if (!menu.contains(e.target)) close(); };
  const shown = items.filter((item, i) => item || (i > 0 && items[i - 1] && i < items.length - 1));
  menu.replaceChildren(...shown.map((item) => {
    if (!item) return h('div.menu-sep', { role: 'separator' });
    if (item.header) return h('div.menu-header', {}, item.label);
    return h(`button.menu-item${item.danger ? '.danger' : ''}${item.strong ? '.strong' : ''}`, {
      role: 'menuitem',
      disabled: busy && !item.strong,
      onclick: () => { close(); item.run(); },
    }, item.label);
  }));
  menu.onkeydown = (e) => {
    const buttons = [...menu.querySelectorAll('.menu-item:not(:disabled)')];
    const at = buttons.indexOf(document.activeElement);
    if (e.key === 'Escape' || e.key === 'Tab') { e.preventDefault(); close(); }
    else if (e.key === 'ArrowDown') { e.preventDefault(); buttons[(at + 1) % buttons.length]?.focus(); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); buttons[(at - 1 + buttons.length) % buttons.length]?.focus(); }
  };
  menu.hidden = false;
  // Keep the menu inside the window.
  const box = menu.getBoundingClientRect();
  menu.style.left = `${Math.max(4, Math.min(x, window.innerWidth - box.width - 4))}px`;
  menu.style.top = `${Math.max(4, Math.min(y, window.innerHeight - box.height - 4))}px`;
  menu.querySelector('.menu-item:not(:disabled)')?.focus();
  document.addEventListener('pointerdown', outside, true);
  window.addEventListener('blur', close);
  window.addEventListener('resize', close);
  $('#library').addEventListener('scroll', close);
  $('#sections').addEventListener('scroll', close);
}

/** Notes about the stores, Steam, and controllers, in the content's bottom left corner. */
function renderNotices() {
  const notices = view.warnings.map((w) => h('div.notice', {}, h('strong', {}, 'Note: '), w));
  if (!view.steam.installed) notices.unshift(h('div.notice', {}, h('strong', {}, 'Steam not found. '), 'Install Steam and sign in once; then steambro can add games.'));
  const controller = controllerNotice();
  if (controller) notices.unshift(controller);
  $('#notices').replaceChildren(...notices);
}

function skeleton() {
  $('#tiles-title').textContent = ALL.label;
  $('#library').replaceChildren(...Array.from({ length: 8 }, () =>
    h('div.card', {}, h('div.frame.loading'), h('div.card-name', {}, ' '))));
}

// ---------------------------------------------------------------- game page

// The game page's tabs: [id, label, contents].
const TABS = [['info', 'Info', info], ['artwork', 'Artwork', artwork], ['settings', 'Settings', behavior]];

/** The header, the game's name and what to do in Steam, then the tabs. A gone game has only its shortcut. */
function renderDetail() {
  const g = game(openKey);
  if (!g) return closeGame();
  const page = $('#detail');
  if (g.gone) {
    page.replaceChildren(hero(g), summary(g));
    return;
  }
  // Switching tabs keeps the tabs where they were when they stuck to the top.
  const tabsTop = page.querySelector('.tabs')?.offsetTop;
  const scroll = tabsTop === undefined ? 0 : Math.min(page.scrollTop, tabsTop);
  const warn = g.steamInputOff || g.steamOverlayOff;
  const contents = TABS.find(([id]) => id === detailTab)[2](g);
  page.replaceChildren(hero(g), summary(g),
    h('div.tabs', { role: 'tablist', 'aria-label': g.name },
      ...TABS.map(([id, label]) => h(`button.tab${id === detailTab ? '.on' : ''}`, {
        role: 'tab',
        'aria-selected': String(id === detailTab),
        onclick: () => { detailTab = id; renderDetail(); },
      }, label, id === 'settings' && warn && h('span.tab-warn', { title: 'Steam turned something off for this game' }, '!')))),
    h('div.tab-page', { role: 'tabpanel' }, contents || h('div.section', {}, h('p', {}, 'Reading the settings…'))));
  page.scrollTop = scroll;
}

/** The top of the game page, laid out the way Steam shows header and logo. It moves the window too. */
function hero(g) {
  const title = h('div.title-fallback', {}, g.name);
  const logo = frameless(g, 'logo', () => logo.replaceWith(title));
  return h('div.hero', { 'data-drag': true },
    frame(g, 'hero'),
    logo);
}

/** A plain image for the header's logo overlay; `onMissing` replaces it with the game's name. */
function frameless(g, slot, onMissing) {
  const img = h('img.logo', { alt: '' });
  img.onerror = onMissing;
  img.src = slotImage(g, slotOf(g, slot)).url;
  return img;
}

/** The game's cover, name, store and status, and what can be done with it in Steam. */
function summary(g) {
  const actions = [];
  let note = null;
  if (g.gone) {
    note = 'Not installed any more. Its shortcut and artwork are still in Steam.';
    actions.push(h('button.danger', { onclick: () => changeInSteam([g], 'remove'), disabled: busy }, 'Remove from Steam'));
  } else if (g.status === 'none') {
    actions.push(h('button.primary', { onclick: () => changeInSteam([g]), disabled: busy || !view.steam.account }, 'Add to Steam'));
  } else if (g.status === 'user') {
    note = 'You added this game to Steam yourself. steambro leaves that shortcut alone.';
  } else {
    if (g.outdated) {
      note = 'Added by steambro. Its shortcut starts the game an older way.';
      actions.push(h('button.primary', { onclick: () => changeInSteam([g], 'update'), disabled: busy }, 'Update shortcut'));
    } else {
      note = 'Added by steambro.';
    }
    actions.push(h('button.ghost.danger', { onclick: () => changeInSteam([g], 'remove'), disabled: busy }, 'Remove from Steam'));
  }
  return h('div.summary', {},
    frame(g, 'cover', { fallback: h('div.placeholder', {}, g.name) }),
    h('div.summary-text', {},
      h('h1', {}, g.name),
      meta(g)),
    h('div.summary-actions', {}, ...actions, note && h('div.note', {}, note)));
}

/** The Info tab: where the game is, how it starts, and where its artwork comes from. */
function info(g) {
  const row = (term, value, title) => [h('dt', {}, term), h('dd', { title }, value)];
  return h('div.section.compact', {},
    h('dl.facts', {},
      ...row('Store', h('span.store-name', {}, storeLogo(g.store), sourceLabel(g))),
      ...row('Folder', h('a', { onclick: () => invoke('open_folder', { key: g.key }) }, g.folder), g.folder),
      ...row('Starts with', g.launch, g.launch),
      ...row('Artwork from', g.artSource)),
    h('div.info-actions', {},
      h('button', { onclick: () => invoke('open_folder', { key: g.key }) }, 'Open folder'),
      g.store === 'exe' && h('button.ghost', { onclick: () => forgetExe(g), disabled: busy }, 'Forget this exe')));
}

/** Where a game comes from, for display: `Xbox`, or `Xbox app` for an app like Netflix. */
function sourceLabel(g) {
  return g.isApp ? `${g.source} app` : g.source;
}

/** The store of a game, with its behavior, from the settings view. */
function storeOf(g) {
  return settingsView && settingsView.stores.find((st) => st.id === g.store);
}

/** A select of `options` ([value, label]); `onchange` gets the chosen value. */
function select(label, options, value, onchange) {
  return h('select', { 'aria-label': label, onchange: (e) => onchange(e.target.value) },
    ...options.map(([v, text]) => h('option', { value: v, selected: v === value }, text)));
}

/** A row of the settings: a title, a line of help, and its control (a switch goes first). */
function settingRow(title, help, control) {
  const isSwitch = control.classList.contains('switch');
  return h('label.setting', {}, isSwitch && control, h('span', {}, h('strong', {}, title), h('small', {}, help)), !isSwitch && control);
}

function toggle(checked, onchange) {
  return h('input.switch', { type: 'checkbox', role: 'switch', checked, onchange: (e) => onchange(e.target.checked) });
}

const APP_RUNNING = [['ask', 'Ask every time'], ['restart', 'Restart it through Steam'], ['leave', 'Leave it']];

/** Steam's overlay key, as set in Steam. */
function overlayKey() {
  return (settingsView && settingsView.steamOverlay && settingsView.steamOverlay.key) || 'Shift+Tab';
}

/** What steambro does in Steam's place while Steam can't see a game: [setting, title, help]. */
function standInParts() {
  return [
    ['bridge', "Copy Steam's controllers",
      'Passes Steam Input on as virtual Xbox 360 controllers, through the ViGEmBus driver. Needs the game\'s controller layout.'],
    ['layout', "Use the game's controller layout",
      "Steam uses the game's Steam Input layout while it plays, whatever window is in front. Off, Steam keeps its desktop layout: mouse and keyboard."],
    ['overlay', "Steam's overlay over the game",
      "Opens with the Steam button, in a window of steambro's over the game. Steam's notifications don't show."],
    ['overlayKeys', `Open the overlay with ${overlayKey()}`,
      "Steam's overlay key, as set in Steam. steambro takes it only while the game is in front."],
    ['exitEndsGame', 'Exit game in Steam ends the game',
      "Exit game and Stop end the game, as they end Steam's own games."],
  ];
}

/** The title and help of the rows about a store's own app, like Battle.net. */
function appParts(app) {
  return {
    appRunning: [`When ${app} is already running`,
      `Started outside Steam, ${app} hides the games it starts from Steam: no overlay, no Steam Input. steambro can restart it through Steam first.`],
    closeApp: [`Close ${app} after the game`,
      `Only when steambro opened it. Steam counts ${app} as part of the game, so it shows the game as running until ${app} closes.`],
  };
}

const FULL_SCREEN_HELP = 'No title bar and no taskbar, like on a TV. In a web app like Netflix, F11 leaves full screen.';

const START_WAITS = [30, 60, 90, 120, 300, 600, 1200];
const END_WAITS = [0, 3, 8, 15, 30, 60, 120];

/** A duration in seconds for display: `90 s`, `2 min`. */
function seconds(s) {
  return s >= 120 && s % 60 === 0 ? `${s / 60} min` : `${s} s`;
}

/** The choices of a timing select, with `current` added when it isn't one of them. */
function timings(choices, current, format) {
  const all = [...new Set([...choices, current])].sort((a, b) => a - b);
  return all.map((v) => [String(v), format(v)]);
}

/** The game's own fixes: its overrides of its store's behavior (see the settings page). */
function behavior(g) {
  const store = storeOf(g);
  if (!store) return null;
  const own = g.gameSettings;
  const save = async (change) => {
    const next = { ...own, ...change };
    try {
      await invoke('set_game_settings', { key: g.key, settings: next });
      g.gameSettings = next;
    } catch (error) {
      toast(`steambro couldn't save that: ${error}`, 'error');
    }
    renderDetail();
  };
  const onOff = (on) => (on ? 'on' : 'off');
  // A switch of the store's, which the game can turn on or off for itself.
  const inherited = (field, title, help) => {
    const appDefault = g.isApp && (field === 'bridge' || (field === 'layout' && own.bridge !== true));
    let first = appDefault ? 'Off, as for apps' : `As for ${store.name} (${onOff(store.settings[field])})`;
    if (g.isApp && field === 'layout' && own.bridge === true) first = 'On, with the bridge';
    if (!g.isApp && field === 'fullScreen') first = 'Off, as for games';
    return settingRow(title, help,
      select(title, [['', first], ['on', 'On'], ['off', 'Off']], own[field] === null ? '' : onOff(own[field]),
        (v) => save({ [field]: v === '' ? null : v === 'on' })));
  };
  const rows = [];
  if (store.app) {
    const parts = appParts(store.app);
    const storeChoice = APP_RUNNING.find(([v]) => v === store.settings.appRunning)[1].toLowerCase();
    rows.push(settingRow(...parts.appRunning,
      select('When the app is already running', [['', `As for ${store.name} (${storeChoice})`], ...APP_RUNNING], own.appRunning || '',
        (v) => save({ appRunning: v || null }))));
    rows.push(inherited('closeApp', ...parts.closeApp));
  }
  if (store.needsBridge || store.app) {
    rows.push(h('p.subhead', {}, store.needsBridge
      ? (g.isApp ? 'Windows starts this app, so Steam can\'t see it. steambro stands in for Steam:' : 'Windows starts this game, so Steam can\'t see it. steambro stands in for Steam:')
      : `When ${store.app} stays outside Steam, Steam can't see the game. Then steambro stands in for Steam:`));
    for (const [field, title, help] of standInParts()) {
      const appHelp = g.isApp && field === 'bridge'
        ? "Off for apps: Steam's desktop layout gives them mouse and keyboard, which apps like Netflix understand. Turn it on only for an app that reads controllers."
        : help;
      rows.push(inherited(field, title, appHelp));
    }
  }
  rows.push(h('p.subhead', {}, 'Window:'));
  rows.push(inherited('fullScreen', 'Start in full screen', FULL_SCREEN_HELP));
  const launch = settingsView.settings.launch;
  rows.push(h('p.subhead', {}, 'Waits for this game:'));
  const startDefault = Math.max(launch.startTimeoutS, g.store === 'epic' ? 300 : 0);
  rows.push(settingRow('Wait for the game to start', 'After its store started it. Some games update or show a launcher first.',
    select('Wait for the game to start', [['', `As for all games (${seconds(startDefault)})`], ...timings(START_WAITS, own.startTimeoutS ?? startDefault, seconds)],
      own.startTimeoutS === null ? '' : String(own.startTimeoutS), (v) => save({ startTimeoutS: v === '' ? null : Number(v) }))));
  rows.push(settingRow('The game has ended after', 'No process of it for this long. A game that restarts itself, like after an update, needs longer.',
    select('The game has ended after', [['', `As for all games (${seconds(launch.endGraceS)})`], ...timings(END_WAITS, own.endGraceS ?? launch.endGraceS, seconds)],
      own.endGraceS === null ? '' : String(own.endGraceS), (v) => save({ endGraceS: v === '' ? null : Number(v) }))));
  return h('div.section.compact', {},
    h('p', {}, "steambro's automatic parts for this game. Each follows its store's setting until you change it here."),
    ...rows,
    g.steamInputOff && checkRow({
      state: 'warn',
      name: 'Steam Input is off for this game in Steam',
      text: "Steam doesn't pass controllers on while it's off. To change it, open the game's Properties in Steam, then Controller.",
    }),
    g.steamOverlayOff && checkRow({
      state: 'warn',
      name: "Steam's overlay is off for this game in Steam",
      text: "To change it, open the game's Properties in Steam and turn on \"Enable the Steam Overlay while in-game\".",
    }));
}

function artwork(g) {
  const editable = g.status !== 'user';
  const intro = {
    none: `Images from ${g.artSource}. They go into Steam when you add the game; adjust or replace any of them first if you like.`,
    steambro: 'Changes here update Steam right away. Steam may need a restart to show them.',
    user: 'Steam shows the images of your own shortcut. steambro manages the artwork of the games it adds.',
  }[g.status];
  return h('div.section', {},
    h('p', {}, intro),
    h('div.slots', {}, ...g.slots.map((s) => slotTile(g, s, editable))));
}

function slotState(g, s) {
  if (g.status === 'user') return s.inSteam ? ['In Steam', 'ok'] : ['Steam’s default', ''];
  switch (s.placement) {
    case 'written': return [s.own ? 'Your image, in Steam' : 'In Steam', 'ok'];
    case 'kept': return ['Steam already had one; kept', ''];
    case 'pending': return ['Download failed; tries again next start', 'warn'];
    case 'unavailable': return [`${g.artSource} has none`, ''];
    default: return [s.own ? 'Your image' : `From ${g.artSource}`, ''];
  }
}

function slotTile(g, s, editable) {
  const [text, tone] = slotState(g, s);
  const state = h(`span.slot-state${tone ? '.' + tone : ''}`, {}, text);
  let hasImage = true;
  const picture = frame(g, s.slot, {
    fallback: h('div.placeholder', {}, s.slot === 'logo' ? 'No logo; Steam shows the name' : 'No image'),
    onMissing: () => {
      hasImage = false;
      if (!s.placement && !s.own) state.textContent = `${g.artSource} has none`;
      const adjust = tile.querySelector('[data-action="adjust"]');
      if (adjust) adjust.remove();
    },
  });
  const actions = editable ? h('div.slot-actions', {},
    s.shape && !(s.placement === 'kept') && h('button', { 'data-action': 'adjust', onclick: () => hasImage && openCropper(g, s) }, 'Adjust'),
    h('button', { onclick: () => pickFile(g, s) }, 'Replace…'),
    (s.own || s.crop || s.placement === 'kept') && h('button.ghost', { onclick: () => resetSlot(g, s) }, s.placement === 'kept' ? 'Use steambro’s' : 'Reset'),
  ) : null;
  const tile = h('div.slot', { 'data-slot': s.slot },
    h('div.slot-head', {}, h('strong', {}, s.label), state),
    picture,
    actions);
  if (editable) {
    tile.addEventListener('dragover', (e) => { e.preventDefault(); tile.classList.add('drop'); });
    tile.addEventListener('dragleave', () => tile.classList.remove('drop'));
    tile.addEventListener('drop', (e) => {
      e.preventDefault();
      tile.classList.remove('drop');
      const file = e.dataTransfer.files[0];
      if (file) replaceWith(g, s, file);
    });
  }
  return tile;
}

// ---------------------------------------------------------------- artwork edits

/** Edits overwrite a slot where Steam had its own image, so ask first. */
async function confirmOverwrite(g, s) {
  if (s.placement !== 'kept') return true;
  return ask({
    title: `Replace Steam’s ${s.label}?`,
    text: 'Steam already had its own image here when steambro added the game, so steambro left it alone. Saving replaces it.',
    confirm: 'Replace it',
  });
}

function pickFile(g, s) {
  const input = $('#file');
  input.value = '';
  input.onchange = () => input.files[0] && replaceWith(g, s, input.files[0]);
  input.click();
}

async function replaceWith(g, s, file) {
  if (!/^image\/(png|jpeg|webp)$/.test(file.type)) return toast('Use a PNG, JPEG or WebP image.', 'error');
  if (!(await confirmOverwrite(g, s))) return;
  const bytes = new Uint8Array(await file.arrayBuffer());
  await change(() => invoke('replace_art', bytes, { headers: { 'x-game': encodeURIComponent(g.key), 'x-slot': s.slot } }),
    `${capitalize(s.label)} replaced.`);
}

async function resetSlot(g, s) {
  if (!(await confirmOverwrite(g, s))) return;
  await change(() => invoke('reset_art', { key: g.key, slot: s.slot }), `${capitalize(s.label)} reset.`);
}

async function change(action, message) {
  try {
    await action();
    toast(message, 'ok');
  } catch (error) {
    toast(String(error), 'error');
  }
  artVersion = Date.now();
  await load();
}

/** Drag the box to move the crop; the slider or the mouse wheel zooms. */
function openCropper(g, s) {
  const dialog = $('#dialog');
  const img = h('img', { alt: '', draggable: 'false' });
  const box = h('div.crop-box');
  const zoom = h('input', { type: 'range', min: '1', max: '4', step: '0.01', value: '1' });
  const previewImg = h('img', { alt: '' });
  const preview = h('div.frame', {}, previewImg);
  preview.style.setProperty('--shape', `${s.shape[0]} / ${s.shape[1]}`);
  let crop = null;
  let full = null;
  let standard = null;

  const place = () => {
    const width = img.clientWidth;
    const height = img.clientHeight;
    Object.assign(box.style, { left: `${crop.x * width}px`, top: `${crop.y * height}px`, width: `${crop.w * width}px`, height: `${crop.h * height}px` });
    showCrop(previewImg, crop);
  };
  const clamp = () => {
    crop.x = Math.min(Math.max(crop.x, 0), 1 - crop.w);
    crop.y = Math.min(Math.max(crop.y, 0), 1 - crop.h);
  };
  const setZoom = (z) => {
    const cx = crop.x + crop.w / 2;
    const cy = crop.y + crop.h / 2;
    crop.w = full.w / z;
    crop.h = full.h / z;
    crop.x = cx - crop.w / 2;
    crop.y = cy - crop.h / 2;
    clamp();
    place();
  };

  img.onload = () => {
    // Zoom 1 is the largest crop; "Center it" and an unchanged crop mean the default.
    full = defaultCrop(img.naturalWidth, img.naturalHeight, s.shape, 'center');
    standard = defaultCrop(img.naturalWidth, img.naturalHeight, s.shape, s.anchor);
    crop = { ...(s.crop || standard) };
    zoom.value = String(Math.min(4, full.w / crop.w));
    previewImg.src = img.src;
    requestAnimationFrame(place);
  };
  img.src = `${ART}/source/${s.slot}/${encodeURIComponent(g.key)}?v=${artVersion}`;

  box.addEventListener('pointerdown', (e) => {
    box.setPointerCapture(e.pointerId);
    const start = { x: e.clientX, y: e.clientY, cx: crop.x, cy: crop.y };
    const move = (m) => {
      crop.x = start.cx + (m.clientX - start.x) / img.clientWidth;
      crop.y = start.cy + (m.clientY - start.y) / img.clientHeight;
      clamp();
      place();
    };
    const up = () => { box.removeEventListener('pointermove', move); box.removeEventListener('pointerup', up); };
    box.addEventListener('pointermove', move);
    box.addEventListener('pointerup', up);
  });
  zoom.addEventListener('input', () => setZoom(Number(zoom.value)));
  const stage = h('div.stage', {}, h('div.stage-inner', {}, img, box));
  stage.addEventListener('wheel', (e) => {
    e.preventDefault();
    const z = Math.min(4, Math.max(1, Number(zoom.value) * (e.deltaY < 0 ? 1.08 : 1 / 1.08)));
    zoom.value = String(z);
    setZoom(z);
  }, { passive: false });

  const save = async () => {
    dialog.close();
    // Saving the untouched default stores no crop, so the default keeps following the image.
    const same = standard && ['x', 'y', 'w', 'h'].every((k) => Math.abs(crop[k] - standard[k]) < 1e-4);
    await change(() => invoke('set_crop', { key: g.key, slot: s.slot, crop: same ? null : crop }), `${capitalize(s.label)} updated.`);
  };
  dialog.replaceChildren(
    h('div.dialog-body', {},
      h('h3', {}, `Adjust ${s.label}`),
      h('p', {}, 'Drag the frame to choose what Steam shows. Zoom with the slider or the mouse wheel.'),
      h('div.cropper', {}, stage,
        h('div.crop-side', {},
          h('label', {}, 'Zoom', zoom),
          h('label', {}, 'Preview', preview)))),
    h('div.dialog-actions', {},
      h('button.ghost.start', { onclick: () => { crop = { ...standard }; zoom.value = String(full.w / standard.w); place(); } }, 'Back to default'),
      h('button', { onclick: () => dialog.close() }, 'Cancel'),
      h('button.primary', { onclick: save }, 'Save')));
  dialog.showModal();
}

// ---------------------------------------------------------------- adding games

function accountName(account) {
  return account.name || `account ${account.id}`;
}

// What the window says when it adds games, updates shortcuts that start their games an older way,
// and removes steambro's shortcuts. All three go through `change_steam`.
const WORDS = {
  add: {
    ask: (games) => (games.length > 1 ? `Add ${games.length} games to Steam?` : `Add ${games[0].name} to Steam?`),
    explain: (running) => (running
      ? 'Steam has to be closed while its shortcuts change. steambro gets the artwork first, then closes Steam for a moment, adds the games, and starts Steam again.'
      : 'steambro adds a shortcut with artwork for each game.'),
    confirm: (running) => (running ? 'Restart Steam and add' : 'Add'),
    working: (games) => (games.length === 1 ? `Adding ${games[0].name} to Steam` : `Adding ${games.length} games to Steam`),
    stage: 'Getting artwork…',
    done: (n, games) => (games.length === 1 ? `Added ${games[0].name} to Steam` : `Added ${n} ${n === 1 ? 'game' : 'games'} to Steam`),
    partly: (n, of) => `Added ${n} of ${of} games`,
    failed: 'Couldn’t add the games',
    after: 'Steam is starting again. The games show up in your library.',
  },
  update: {
    ask: (games) => (games.length > 1 ? `Update ${games.length} shortcuts?` : `Update the shortcut of ${games[0].name}?`),
    explain: (running) => `${running ? 'Steam has to be closed for a moment while its shortcuts change. ' : ''}steambro points each shortcut at the way it starts the game now. The games keep their artwork and playtime.`,
    confirm: (running) => (running ? 'Restart Steam and update' : 'Update'),
    working: (games) => (games.length === 1 ? `Updating the shortcut of ${games[0].name}` : `Updating ${games.length} shortcuts`),
    stage: '',
    done: (n, games) => (games.length === 1 ? `Updated the shortcut of ${games[0].name}` : `Updated ${n} ${n === 1 ? 'shortcut' : 'shortcuts'}`),
    partly: (n, of) => `Updated ${n} of ${of} shortcuts`,
    failed: 'Couldn’t update the shortcuts',
    after: 'Steam is starting again.',
  },
  remove: {
    ask: (games) => (games.length > 1 ? `Remove ${games.length} games from Steam?` : `Remove ${games[0].name} from Steam?`),
    explain: (running) => `steambro deletes its shortcut and the artwork it wrote. The game stays installed.${running ? ' Steam has to be closed for a moment while its shortcuts change.' : ''}`,
    confirm: (running) => (running ? 'Restart Steam and remove' : 'Remove'),
    danger: true,
    working: (games) => (games.length === 1 ? `Removing ${games[0].name} from Steam` : `Removing ${games.length} games from Steam`),
    stage: '',
    done: (n, games) => (games.length === 1 ? `Removed ${games[0].name} from Steam` : `Removed ${n} ${n === 1 ? 'game' : 'games'} from Steam`),
    partly: (n, of) => `Removed ${n} of ${of} games`,
    failed: 'Couldn’t remove the games',
    after: 'Steam is starting again.',
  },
};

/**
 * Adds games to Steam (`mode` 'add'), updates their outdated shortcuts ('update'), or removes
 * steambro's shortcuts ('remove'). Resolves to true when the change ran, false when the user
 * cancelled.
 */
async function changeInSteam(games, mode = 'add') {
  if (busy || games.length === 0) return false;
  const words = WORDS[mode];
  const { running, account, accounts } = view.steam;
  const several = games.length > 1;
  const choices = accounts.length > 1 ? [
    { value: 'current', label: `Only ${accountName(account)}`, detail: 'The account that signed in last' },
    { value: 'all', label: 'All Steam accounts', detail: accounts.map(accountName).join(', ') },
  ] : null;
  let allAccounts = false;
  if (several || running || choices || mode !== 'add') {
    const answer = await ask({
      title: words.ask(games),
      text: words.explain(running),
      list: several ? games.map((g) => g.name) : null,
      account: choices ? null : accountName(account),
      choices,
      confirm: words.confirm(running),
      danger: words.danger,
    });
    if (!answer) return false;
    allAccounts = answer === 'all';
  }
  busy = true;
  render();
  const panel = openProgress(games, allAccounts ? accounts : [account], words);
  let outcome = null;
  let failure = null;
  try {
    outcome = await invoke('change_steam', { keys: games.map((g) => g.key), request: mode === 'remove' ? 'remove' : 'add', allAccounts, restartSteam: running });
  } catch (error) {
    failure = String(error);
  }
  if (outcome && outcome.needsRestart) {
    // Steam started after the window last looked: ask again, with the restart.
    panel.close();
    busy = false;
    view.steam.running = true;
    return changeInSteam(games, mode);
  }
  panel.finish(failure);
  busy = false;
  artVersion = Date.now();
  await load();
  return true;
}

// ---------------------------------------------------------------- progress

// The panel that covers the window while Steam's shortcuts change. It follows the
// `progress-game` and `progress-steam` events (see change_steam in src/app/commands.rs) and ends
// with a cloud for the result.
let progress = null;

function openProgress(games, accounts, words) {
  const dialog = $('#progress');
  const cloud = h('img.progress-cloud.working', { src: 'logo-wink.svg', alt: '' });
  const title = h('h3', {}, words.working(games));
  const stage = h('p.progress-stage', {}, words.stage);
  const rows = new Map(games.map((g) => [g.key, progressRow(g)]));
  const done = h('button.primary', { disabled: true, onclick: () => dialog.close() }, 'Done');
  dialog.replaceChildren(
    h('div.dialog-body', {},
      h('div.progress-head', {}, cloud,
        h('div', {}, title, h('div.progress-account', {}, accounts.length === 1 ? `Steam account: ${accountName(accounts[0])}` : 'All Steam accounts'))),
      stage,
      h('div.progress-list', {}, ...[...rows.values()].map((row) => row.node))),
    h('div.dialog-actions', {}, done));
  // Escape can't close it while games are being added.
  dialog.oncancel = (e) => { if (done.disabled) e.preventDefault(); };
  dialog.showModal();

  progress = {
    game(key, step) {
      const row = rows.get(key);
      if (!row) return;
      if (step.kind === 'artwork') {
        const s = slotOf(row.game, step.slot);
        row.set('working', `Getting artwork: ${s ? s.label : step.slot} (${step.number} of ${step.of})`);
      } else if (step.kind === 'ready') {
        row.set('ready', 'Artwork ready');
      } else if (step.kind === 'added') {
        row.set('done', step.later ? `Added; ${step.later === 1 ? '1 image downloads' : `${step.later} images download`} at the next start` : 'Added');
      } else if (step.kind === 'updated') {
        row.set('done', 'Shortcut updated');
      } else if (step.kind === 'removed') {
        row.set('done', 'Removed');
      } else if (step.kind === 'alreadyInSteam') {
        row.set('skipped', 'Already in Steam');
      } else if (step.kind === 'notInSteam') {
        row.set('skipped', 'Not in Steam');
      } else if (step.kind === 'failed') {
        row.set('failed', step.error);
      }
    },
    steam(action) {
      stage.textContent = { closing: 'Closing Steam…', writing: 'Writing Steam’s shortcuts…', starting: 'Starting Steam again…' }[action];
    },
  };

  return {
    close() {
      progress = null;
      dialog.close();
    },
    finish(failure) {
      progress = null;
      const all = [...rows.values()];
      // Games the add never reached, when it stopped early.
      for (const row of all) if (failure && !['done', 'skipped', 'failed'].includes(row.state)) row.set('failed', 'Not changed');
      const count = (state) => all.filter((row) => row.state === state).length;
      const added = count('done');
      const failed = count('failed');
      const mood = failed || failure ? 'glance' : added ? 'squeeze' : 'wink';
      cloud.classList.remove('working');
      cloud.src = `logo-${mood}.svg`;
      // Games that were already in Steam don't count against the result.
      const wanted = all.length - count('skipped');
      if (failed || failure) title.textContent = added ? words.partly(added, wanted) : words.failed;
      else if (added) title.textContent = words.done(added, games);
      else title.textContent = 'Nothing to change';
      stage.textContent = failure || (added && view.steam.running ? words.after : '');
      stage.classList.toggle('error', Boolean(failure));
      if (mood !== 'wink') setMoment(mood, mood === 'glance' ? 9000 : 4000);
      done.disabled = false;
      done.focus();
    },
  };
}

function progressRow(g) {
  const icon = h('span.row-icon');
  const text = h('span.row-text', {}, 'Waiting');
  const node = h('div.progress-row', {},
    frame(g, 'cover', { fallback: h('div.placeholder') }),
    h('div.row-main', {}, h('strong', {}, g.name), text),
    icon);
  const row = {
    game: g,
    node,
    state: 'waiting',
    set(state, message) {
      row.state = state;
      node.dataset.state = state;
      text.textContent = message;
      const face = { done: 'squeeze', failed: 'glance', skipped: 'wink' }[state];
      icon.replaceChildren(...(face ? [h('img', { src: `logo-${face}.svg`, alt: '' })] : []));
    },
  };
  node.dataset.state = 'waiting';
  return row;
}

// ---------------------------------------------------------------- mood

// The cloud in the sidebar and the tray: squeeze after a success, glance at an error, wink
// otherwise. A toast's mood lasts as long as the toast; an error state lasts until it clears.
let moment = null;
let momentTimer = 0;
// The app's mood, also on the tray icon.
let shownMood = 'wink';
// What the logo shows: the mood, or how it reacts to the mouse (`poke`).
let shownFace = 'wink';
let poke = null;

function setMoment(mood, timeout) {
  moment = mood;
  clearTimeout(momentTimer);
  momentTimer = setTimeout(() => { moment = null; showMood(); }, timeout);
  showMood();
}

function showMood() {
  const trouble = loadFailed || (view && !view.steam.installed);
  const mood = moment || (trouble ? 'glance' : 'wink');
  if (mood !== shownMood) {
    shownMood = mood;
    invoke('set_mood', { mood }).catch(() => {});
  }
  const face = poke || mood;
  if (face === shownFace) return;
  shownFace = face;
  const logo = $('#logo');
  logo.src = `logo-${face}.svg`;
  if (poke) return;
  logo.classList.remove('pop');
  void logo.offsetWidth; // restarts the animation
  logo.classList.add('pop');
}

// The cloud in the corner, for fun: it squeezes its eyes while the mouse is over it, and glances
// away while it is pressed. The tray icon keeps the app's mood.
function pokeLogo() {
  const logo = $('#logo');
  for (const face of ['wink', 'glance', 'squeeze']) new Image().src = `logo-${face}.svg`;
  const set = (face) => { poke = face; showMood(); };
  let pressed = false;
  logo.addEventListener('pointerenter', () => set('squeeze'));
  logo.addEventListener('pointerleave', () => { if (!pressed) set(null); });
  logo.addEventListener('pointerdown', (e) => {
    if (e.button !== 0) return;
    pressed = true;
    logo.setPointerCapture(e.pointerId);
    set('glance');
  });
  const release = (e) => {
    pressed = false;
    const over = document.elementsFromPoint(e.clientX, e.clientY).includes(logo);
    set(over ? 'squeeze' : null);
  };
  logo.addEventListener('pointerup', release);
  logo.addEventListener('pointercancel', release);
}

// ---------------------------------------------------------------- title bar

// What a press on a data-drag area leaves to the element under it instead of moving the window.
const INTERACTIVE = 'button, a, input, select, textarea, label, .card, #logo';

/**
 * The window buttons, cut into the content's corner, and moving the window: pressing an empty part
 * marked data-drag (the brand, a game's header, a page's title) moves it, and a double click
 * maximizes it, as Windows' title bar does. Closing keeps steambro in the tray.
 */
function windowControls() {
  const win = window.__TAURI__.window.getCurrentWindow();
  document.addEventListener('mousedown', (e) => {
    if (e.button !== 0 || !e.target.closest('[data-drag]') || e.target.closest(INTERACTIVE)) return;
    if (e.detail === 2) win.toggleMaximize();
    else win.startDragging();
  });
  $('#wc-min').addEventListener('click', () => win.minimize());
  $('#wc-max').addEventListener('click', () => win.toggleMaximize());
  $('#wc-close').addEventListener('click', () => win.close());
  const sync = async () => {
    const maximized = await win.isMaximized();
    document.body.classList.toggle('maximized', maximized);
    $('#wc-max').title = maximized ? 'Restore' : 'Maximize';
    $('#wc-max').setAttribute('aria-label', $('#wc-max').title);
  };
  win.onResized(sync);
  sync();
}

// ---------------------------------------------------------------- settings

// What the settings page shows (settings_view in src/app/commands.rs). The notices use it too.
let settingsView = null;

const KINDS = {
  xbox: 'Xbox controller',
  steam: 'Steam Controller',
  playstation: 'PlayStation controller',
  nintendo: 'Switch Pro controller',
  other: 'other controller',
};

async function loadSettings() {
  try {
    settingsView = await invoke('settings_view');
  } catch (error) {
    if (mode === 'settings') toast(`steambro couldn't read the settings: ${error}`, 'error');
    return;
  }
  if (view) {
    renderNotices();
    if (mode === 'game') renderDetail();
    if (mode === 'tiles' && !everything.length) renderTiles();
  }
  if (mode === 'settings') renderSettings();
}

/** Steam Input's setting for a kind of controller: true on, false off, null Steam's default. */
function steamSupport(kind) {
  if (kind === 'steam') return true;
  const input = settingsView.steamInput;
  const value = input && input[kind === 'other' ? 'generic' : kind];
  return value === null || value === undefined ? null : value !== '0';
}

/** A notice when a connected controller won't reach Xbox games. */
function controllerNotice() {
  const s = settingsView;
  if (!s || !s.stores.some((st) => st.needsBridge && st.games && st.settings.bridge)) return null;
  const copied = s.connected.filter((kind) => kind !== 'xbox');
  const off = copied.find((kind) => steamSupport(kind) === false);
  let text = null;
  if (copied.length && !s.driver.installed) text = `Your ${KINDS[copied[0]]} needs a driver to work in Xbox games.`;
  else if (off) text = `Steam Input is off for ${KINDS[off]}s, so yours won't work in Xbox games.`;
  if (!text) return null;
  return h('div.notice.action', {},
    h('span', {}, h('strong', {}, 'Controllers: '), text),
    h('button', { onclick: openSettings }, 'Settings'));
}

/** One line of the checks: `state` is ok, warn, bad or idle; `action` is [label, onclick]. */
function checkRow({ state, name, text, action }) {
  const mark = { ok: '\u2713', warn: '!', bad: '\u2715', idle: '\u2013' }[state];
  return h(`div.check-row.${state}`, {},
    h('span.mark', { 'aria-hidden': 'true' }, mark),
    h('span', {}, h('strong', {}, name), h('small', {}, text)),
    action && h('button', { onclick: action[1], disabled: busy }, action[0]));
}

function driverCheck(d) {
  if (d.available) return { state: 'ok', name: `ViGEmBus driver ${d.version || ''}`.trim(), text: 'Installed.' };
  if (d.installed) return { state: 'warn', name: 'ViGEmBus driver', text: "Installed, but it doesn't answer. Restart Windows, then look again." };
  return {
    state: 'bad',
    name: 'ViGEmBus driver',
    text: 'Not installed. Controllers other than Xbox controllers need this free driver to work in Xbox games.',
    action: ['Get it', () => openLink('driver')],
  };
}

/** Each kind of controller connected now or used with Steam before, and whether it reaches Xbox games. */
function controllerChecks(s) {
  const kinds = [...new Set([...s.connected, ...s.seen])];
  if (!kinds.length) return [{ state: 'idle', name: 'No controller found', text: 'Connect one to check it here.' }];
  return kinds.map((kind) => {
    const name = `${capitalize(KINDS[kind])}${s.connected.includes(kind) ? ', connected' : ', used with Steam before'}`;
    if (kind === 'xbox') {
      return { state: 'ok', name, text: "The game reads it itself. steambro only keeps Steam's desktop layout from giving its input a second time." };
    }
    if (steamSupport(kind) === false) {
      return {
        state: 'warn',
        name,
        text: `Steam Input is off for ${KINDS[kind]}s in Steam's settings, so Steam doesn't pass it on. Turning it on changes it for all Steam games.`,
        action: ["Open Steam's settings", () => invoke('open_steam_controller_settings').catch((e) => toast(String(e), 'error'))],
      };
    }
    if (!s.driver.available) return { state: 'warn', name, text: 'Reaches Xbox games only through the driver above.' };
    return { state: 'ok', name, text: 'Steam passes it to Xbox games as a virtual Xbox 360 controller.' };
  });
}

function renderSettings() {
  const s = settingsView;
  const head = h('div.settings-head', { 'data-drag': true }, h('h1', {}, 'Settings'));
  if (!s) {
    $('#settings').replaceChildren(head, h('div.section', {}, h('p', {}, 'Reading the settings…')));
    return;
  }
  const d = s.driver;
  const overlayOff = s.steamOverlay && !s.steamOverlay.enabled && checkRow({
    state: 'warn',
    name: "Steam's overlay is off in Steam",
    text: "steambro can't show it over any game. To change it, open Steam's settings, then In game.",
  });
  $('#settings').replaceChildren(head,
    h('div.section', {},
      h('h2', {}, 'Stores'),
      h('p', {}, "Each store starts its games its own way, and some ways hide the game from Steam. Then steambro stands in for Steam. These are its automatic parts, per store; a game's page can change them for that game."),
      overlayOff && h('div.checks', {}, overlayOff),
      ...s.stores.filter((st) => st.games > 0).map(storeCard)),
    h('div.section', {},
      h('h2', {}, 'Steam Input bridge'),
      h('p', {}, "When Steam can't see a game, as with Xbox games, Steam Input can't reach it by itself. Then steambro copies Steam's controllers to virtual Xbox 360 controllers, which the game reads like real ones."),
      h('div.checks', {}, checkRow(driverCheck(d)), ...controllerChecks(s).map(checkRow))),
    h('div.section', {},
      h('h2', {}, 'New games'),
      settingRow('Tell me about new games', 'When a game is installed and isn\'t in Steam yet, a Windows notification offers to add it. steambro looks every 5 minutes while it runs in the tray.',
        toggle(s.settings.newGameNotices, async (on) => {
          try {
            await invoke('save_new_game_notices', { on });
          } catch (error) {
            toast(`steambro couldn't save the settings: ${error}`, 'error');
          }
          loadSettings();
        }))),
    launchSection(s.settings.launch),
    h('div.section', {},
      h('h2', {}, 'About'),
      h('p', {}, `steambro ${s.version}. Copyright 2026 shalamowww. steambro is free software: you can share and change it under the GNU General Public License, version 3. It comes with no warranty.`),
      h('div.links', {},
        h('a', { onclick: () => openLink('license') }, 'License'),
        h('a', { onclick: () => openLink('source') }, 'Source code'))));
}

/** How game launches follow every game: the timings. */
function launchSection(launch) {
  const save = async (change) => {
    try {
      await invoke('save_launch_settings', { launch: { ...launch, ...change } });
    } catch (error) {
      toast(`steambro couldn't save the settings: ${error}`, 'error');
    }
    loadSettings();
  };
  const timing = (field, title, help, choices, format) => settingRow(title, help,
    select(title, timings(choices, launch[field], format), String(launch[field]), (v) => save({ [field]: Number(v) })));
  const ms = (v) => (v < 1000 ? `${v} ms` : `${v / 1000} s`);
  return h('div.section', {},
    h('h2', {}, 'Game launches'),
    h('p', {}, "How steambro follows each game it starts, until the game ends. A game's page can change the waits for that game."),
    timing('checkMs', 'Look for the game every', "How often steambro checks the game's processes. Steam shows the game as running until steambro sees it end.", [250, 500, 1000, 2000, 5000], ms),
    timing('startTimeoutS', 'Wait for the game to start', 'After its store started it. Epic Games games get at least 5 minutes, because the launcher may update first.', START_WAITS, seconds),
    timing('nudgeS', 'Ask Battle.net again after', 'When no game process has appeared by then. Battle.net sometimes misses the first request while it starts.', [10, 20, 30, 60, 120], seconds),
    timing('endGraceS', 'The game has ended after', 'No process of it for this long. Some games close and start again, like after an update.', END_WAITS, seconds),
    timing('questionTimeoutS', 'Wait for an answer', 'To a question at a launch, like whether to restart Battle.net. Without an answer, the game starts as it is.', [30, 60, 120, 300, 600], seconds));
}

/** Adds a game by its exe, chosen in Windows' file dialog, and opens its page. */
async function addExe() {
  try {
    const key = await invoke('add_exe');
    if (!key) return;
    await load();
    if (game(key)) openGame(key);
  } catch (error) {
    toast(`steambro couldn't add that: ${error}`, 'error');
  }
}

async function forgetExe(g) {
  const ok = await ask({
    title: `Forget ${g.name}?`,
    text: g.status === 'steambro' ? 'steambro then offers to remove it from Steam too.' : 'It leaves steambro\'s list. The exe stays where it is.',
    confirm: 'Forget',
  });
  if (!ok) return;
  try {
    await invoke('forget_exe', { key: g.key });
    mode = 'tiles';
    openKey = null;
    await load();
  } catch (error) {
    toast(`steambro couldn't forget it: ${error}`, 'error');
  }
}

function openLink(what) {
  invoke('open_link', { what }).catch((error) => toast(`steambro couldn't open that: ${error}`, 'error'));
}

/** One store's behavior on the settings page. */
function storeCard(st) {
  const save = async (change) => {
    try {
      await invoke('save_store_settings', { store: st.id, settings: { ...st.settings, ...change } });
    } catch (error) {
      toast(`steambro couldn't save the settings: ${error}`, 'error');
    }
    loadSettings();
  };
  const rows = [];
  if (st.app) {
    const parts = appParts(st.app);
    rows.push(settingRow(...parts.appRunning, select('When the app is already running', APP_RUNNING, st.settings.appRunning, (v) => save({ appRunning: v }))));
    rows.push(settingRow(...parts.closeApp, toggle(st.settings.closeApp, (on) => save({ closeApp: on }))));
  }
  if (st.needsBridge || st.app) {
    rows.push(h('p.subhead', {}, st.needsBridge
      ? `Windows starts ${st.name} games, so Steam can't see them. steambro stands in for Steam:`
      : `When ${st.app} stays outside Steam, Steam can't see the game. Then steambro stands in for Steam:`));
    for (const [field, title, help] of standInParts()) {
      rows.push(settingRow(title, help, toggle(st.settings[field], (on) => save({ [field]: on }))));
    }
  }
  if (st.apps) {
    rows.push(h('p.subhead', {}, `${st.apps === 1 ? 'The app' : `The ${st.apps} apps`} from the ${st.name} app, like Netflix:`));
    rows.push(settingRow('Start in full screen', FULL_SCREEN_HELP, toggle(st.settings.fullScreen, (on) => save({ fullScreen: on }))));
  }
  return h('div.store-card', {},
    h('div.store-head', {}, storeLogo(st.id), h('h3', {}, st.name), h('small', {}, st.games === 1 ? '1 game' : `${st.games} games`)),
    rows.length ? rows : h('p.fine', {}, 'Steam starts and sees these games like its own; nothing to fix.'));
}

// ---------------------------------------------------------------- dialogs and toasts

/** A question in the window's dialog; see `choose` in choice.js. */
function ask(options) {
  return choose($('#dialog'), options);
}

/** Shows a message in the corner for a few seconds; an error stays longer. */
function toast(message, kind = '') {
  const timeout = kind === 'error' ? 9000 : 4000;
  const node = h(`div.toast${kind ? '.' + kind : ''}`, {}, message);
  $('#toasts').append(node);
  setTimeout(() => node.remove(), timeout);
  if (kind === 'ok') setMoment('squeeze', timeout);
  if (kind === 'error') setMoment('glance', timeout);
}

function capitalize(text) {
  return text.charAt(0).toUpperCase() + text.slice(1);
}

// ---------------------------------------------------------------- start

/** Looks for games again, with the refresh button turning meanwhile. */
async function rescan() {
  const button = $('#refresh');
  button.classList.add('spinning');
  await load(true);
  button.classList.remove('spinning');
}

$('#refresh').addEventListener('click', rescan);
$('#add-exe').addEventListener('click', addExe);
$('#all-games').addEventListener('click', () => openTiles(ALL.id));
$('#open-settings').addEventListener('click', () => (mode === 'settings' ? closeSettings() : openSettings()));
const search = $('#search');
search.addEventListener('input', () => {
  query = search.value;
  if (view) applyFilter();
});
search.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && search.value) {
    e.stopPropagation();
    search.value = '';
    search.dispatchEvent(new Event('input'));
  }
});
// Up and down move between the sidebar's sections and games.
$('#sections').addEventListener('keydown', (e) => {
  if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
  const items = [...$('#sections').querySelectorAll('.section-link, .game-row')];
  const at = items.indexOf(document.activeElement);
  if (at < 0) return;
  e.preventDefault();
  items[Math.min(Math.max(at + (e.key === 'ArrowDown' ? 1 : -1), 0), items.length - 1)].focus();
});
// The browser's own menu (back, reload, inspect) has no place here; text fields keep theirs.
document.addEventListener('contextmenu', (e) => {
  if (!e.target.closest('input, textarea')) e.preventDefault();
});
document.addEventListener('keydown', (e) => {
  const typing = e.target.closest('input, textarea, select');
  const modal = $('#dialog').open || $('#progress').open || !$('#menu').hidden;
  if (modal) return;
  if (e.key === 'Escape') {
    if (mode === 'settings') closeSettings();
    else if (mode === 'game') closeGame();
    else if (selected.size) clearSelection();
  } else if (!typing && (e.key === '/' || (e.ctrlKey && e.key === 'f'))) {
    // The search is the tiles'; from a game's page, it goes back to them.
    e.preventDefault();
    if (mode !== 'tiles') openTiles(openSection);
    search.focus();
    search.select();
  } else if (!typing && mode === 'tiles' && e.ctrlKey && e.key === 'a') {
    e.preventDefault();
    selectAll();
  }
});
listen('library-changed', () => load());
listen('progress-game', ({ payload }) => progress && progress.game(payload.key, payload.step));
listen('progress-steam', ({ payload }) => progress && progress.steam(payload));
// Look for new games whenever the window comes back from the tray.
listen('window-shown', () => {
  if (!busy) load(true);
  loadSettings();
});
pokeLogo();
windowControls();
skeleton();
load();
loadSettings();
