'use strict';

// steambro's window. Talks to the Rust side through commands (src/app/commands.rs) and loads
// artwork through the `art` URL scheme (src/app/protocol.rs). The sidebar lists the games by
// status; the content shows one status's games as tiles, or a game's page. The settings open in
// a window of their own over it. A controller, or the arrow keys, move through all of it as the
// focus (controller navigation, below).
//
// The page has no rules of its own about stores, settings, or controllers. Rust sends the
// results: the stores and their order (`stores`), what a game inherits (`inherited`), when Steam
// can't see a store's games (`unseen`), a store's values (`values`), whether a store may miss a
// launch request (`asksAgain`), the table of controllers with their virtual controllers and
// Steam's state for each (`controllers`), and whether a connected controller won't reach an Xbox
// game (`controllerAlert`). The page shows them.

// Read once at start: a script that replaces `window.__TAURI__.core.invoke` later doesn't reach the page.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const ART = 'http://art.localhost';
const SVG = 'http://www.w3.org/2000/svg';

// Keys of the texts (ui/i18n): the constants are read before the texts load, so t() runs at render.
const STATUS = {
  steambro: 'game.status_in_steam',
  user: 'game.status_in_steam',
  none: 'game.status_not_in_steam',
};

// The sidebar's sections: what waits for Steam, what's in Steam (and what needs something there),
// then the rest. A game is in the first section it matches; a game the user ignores is only in the
// last one. A section without games doesn't show.
// `label` and `hint` are text keys; Ignored has no hint.
const IGNORED = { id: 'ignored', label: 'sections.ignored', hint: '', test: (g) => g.ignored };
// Games that wait for Steam to restart, or that change in Steam right now. It shows above the
// library, with the button that restarts Steam (renderSteamChanges).
const PENDING = { id: 'pending', label: 'sections.pending', hint: 'sections.pending_hint', test: (g) => isChanging(g.key) };
const SECTIONS = [
  PENDING,
  { id: 'in', label: 'sections.in', hint: 'sections.in_hint', test: (g) => g.status === 'steambro' && !g.outdated && !g.gone },
  { id: 'old', label: 'sections.old', hint: 'sections.old_hint', test: (g) => g.outdated },
  { id: 'gone', label: 'sections.gone', hint: 'sections.gone_hint', test: (g) => g.gone },
  { id: 'none', label: 'sections.none', hint: 'sections.none_hint', test: (g) => !g.gone && g.status === 'none' },
  { id: 'user', label: 'sections.user', hint: 'sections.user_hint', test: (g) => g.status === 'user' },
  IGNORED,
];
const ALL = { id: 'all', label: 'sections.all', hint: 'sections.all_hint' };
// The height of the window buttons' line at the top of the content (--top-bar in style.css).
const TOP_BAR = 32;

const ICONS = {
  fold: 'M9 6l6 6-6 6',
  tiles: 'M4 4h6v6H4zM14 4h6v6h-6zM4 14h6v6H4zM14 14h6v6h-6z',
  plus: 'M12 5v14M5 12h14',
  refresh: 'M20 12a8 8 0 1 1-2.34-5.66M20 4v5h-5',
  // A window, for an added exe or a store without a logo.
  window: 'M4 5h16a1 1 0 0 1 1 1v12a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1zM3 9h18',
  // Sliders, for the settings that are for every game.
  sliders: 'M4 21v-7M4 10V3M12 21v-9M12 8V3M20 21v-5M20 12V3M1 14h6M9 8h6M17 16h6',
  // A gamepad with two grips, for the Controllers page of the settings.
  gamepad: 'M7 7h10a5 5 0 0 1 5 5v2a4 4 0 0 1-7.2 2.4L14 15.5h-4l-.8.9A4 4 0 0 1 2 14v-2a5 5 0 0 1 5-5zM8 10v4M6 12h4M16 11h.01M18.5 13h.01',
  // A heart, for "Support the project" in the settings.
  heart: 'M12 20.5l-1.3-1.2C5.8 14.9 3 12.3 3 9a4.6 4.6 0 0 1 4.6-4.6c1.7 0 3.3.8 4.4 2.1a5.7 5.7 0 0 1 4.4-2.1A4.6 4.6 0 0 1 21 9c0 3.3-2.8 5.9-7.7 10.3z',
  // A globe, for the Language setting and its Auto choice.
  globe: 'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18',
};

let view = null;
// The games found, then the Steam shortcuts of games gone from the PC (`goneGame`).
let everything = [];
// What the content shows: 'tiles' for a section's games, or 'game' for a game's page.
let mode = 'tiles';
// The section the tiles show, or ALL.
let openSection = ALL.id;
let openKey = null;
// The game page's tab, kept from game to game.
let detailTab = 'info';
// The folded sections of the sidebar, by ID, remembered for the next start. Ignored starts folded.
const folded = new Set(remembered('folded', [IGNORED.id]));
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
// A change in Steam while it runs (runChange): `stage`, what Steam is doing, and a row per game by
// key, `{ g, action, state, text }`. A game leaves it as soon as it's done, for its new section.
let changing = null;
// Games that just arrived in their new section, which their rows show for a moment.
const arrived = new Set();
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
    toast(t('toast.library_failed', { error }), 'error');
    return;
  }
  everything = [...view.games, ...view.gone.map(goneGame)];
  render();
}

/** The Steam shortcut of a game gone from the PC, as a game. It shows Steam's artwork (`slotImage`). */
function goneGame(g) {
  return { ...g, source: g.source || t('store.unknown'), gone: true, status: 'steambro', outdated: false, isApp: false };
}

function game(key) {
  return everything.find((g) => g.key === key);
}

/**
 * The game waits for Steam to restart, or changes in Steam right now. A change that writes Steam's
 * shortcuts has every waiting game among its rows, and a game leaves them when it's done, before
 * the window reads the waiting list again.
 */
function isChanging(key) {
  if (changing && !changing.later) return changing.rows.has(key);
  return Boolean((changing && changing.rows.has(key)) || (view && view.pending.some((p) => p.key === key)));
}

function sectionOf(g) {
  return g.ignored ? IGNORED : SECTIONS.find((s) => s.test(g));
}

function section(id) {
  return SECTIONS.find((s) => s.id === id) || ALL;
}

/** The section's games; ALL has every game the user doesn't ignore. */
function sectionGames(id) {
  return id === ALL.id ? everything.filter((g) => !g.ignored) : everything.filter((g) => sectionOf(g).id === id);
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

/**
 * What the slot shows: Steam's own image where steambro left Steam's alone or the game is gone
 * from the PC, else steambro's source.
 */
function slotImage(g, s) {
  const steams = s.inSteam && (s.placement === 'kept' || g.status === 'user' || g.gone);
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

/**
 * A filled SVG path in a 24 by 24 box, like a store's logo; `outline` draws it as a line. Icons are
 * SVG elements, not images: the content security policy allows images only from the app and the
 * `art` scheme, so a `data:` image doesn't load.
 */
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

/**
 * Steam, beside the window buttons: whether it runs, which decides whether a change needs a
 * restart. The account's name shows only when more than one Steam account has signed in on this
 * PC, so it's clear whose library changes.
 */
function renderAccount() {
  const { account, accounts, installed, running } = view.steam;
  const name = account ? accountName(account) : null;
  const state = t(!installed ? 'account.steam_missing' : !account ? 'account.none_yet' : running ? 'account.steam_running' : 'account.steam_closed');
  const label = account && accounts.length > 1 ? name : !installed || !account ? state : t(running ? 'account.running' : 'account.closed');
  const parts = [pathIcon(STEAM_LOGO, 'steam-logo'), h('span.account-name', {}, label)];
  if (account) parts.push(h(`span.dot${running ? '.on' : ''}`));
  $('#account').replaceChildren(...parts);
  $('#account').title = account ? t(running ? 'account.title_running' : 'account.title_closed', { account: name }) : state;
}

/** Shows what the content is set to: a section's tiles, or a game's page. */
function renderContent() {
  if (mode === 'game' && !game(openKey)) mode = 'tiles';
  $('#tiles').hidden = mode !== 'tiles';
  $('#detail').hidden = mode !== 'game';
  $('#back').hidden = mode !== 'game';
  if (mode === 'tiles') renderTiles();
  else renderDetail();
  renderPadHints();
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
  padLand('#library .card:not([hidden])');
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
  // A gone game has no tabs, only its summary.
  padLand('.tabs .tab.on', '.summary-actions button');
}

/** Back from a game's page to the tiles of its section, wherever the page was opened from. */
function closeGame() {
  const g = game(openKey);
  openTiles(g ? sectionOf(g).id : openSection);
}

// ---------------------------------------------------------------- sidebar

/** The sections with their games, under the brand. */
function renderSidebar() {
  const nav = $('#sections');
  const scroll = nav.scrollTop;
  $('#all-count').textContent = String(sectionGames(ALL.id).length);
  const allOpen = mode === 'tiles' && openSection === ALL.id;
  $('#all-games').classList.toggle('open', allOpen);
  const groups = SECTIONS.filter((s) => s !== PENDING).map((s) => [s, sectionGames(s.id)]).filter(([, games]) => games.length);
  nav.replaceChildren(...(groups.length
    ? groups.map(([s, games]) => sectionGroup(s, games))
    : [h('p.side-empty', {}, view && !loadFailed ? t('sidebar.no_games') : '')]));
  nav.scrollTop = scroll;
  renderSteamChanges();
  renderControllerAlert();
  restorePadFocus();
}

/**
 * Above the library: the games that wait for Steam to restart, with the button that restarts it,
 * and while a change runs, what Steam is doing and how far each game is. Done games move to their
 * new section at once.
 */
function renderSteamChanges() {
  const box = $('#steam-changes');
  const games = view ? sectionGames(PENDING.id) : [];
  box.hidden = !games.length;
  if (!games.length) {
    box.replaceChildren();
    return;
  }
  const working = Boolean(changing);
  const open = mode === 'tiles' && openSection === PENDING.id;
  const link = h(`button.section-link.tone-pending${open ? '.open' : ''}`, {
    title: t('sidebar.show_as_tiles', { count: games.length }),
    'aria-current': open ? 'page' : null,
    'data-section': PENDING.id,
    onclick: () => openTiles(PENDING.id),
  }, h('span.section-label', {}, t(working && !changing.later ? 'changes.changing_title' : PENDING.label)), h('span.count', {}, String(games.length)), svgIcon(ICONS.tiles, 'tiles-icon'));
  const stage = working && changing.stage && h('p.changes-stage', {}, h('span.spinner', { 'aria-hidden': 'true' }), changing.stage);
  const restart = !working && h('button.restart-steam', { onclick: restartForWaiting, disabled: busy },
    svgIcon(ICONS.refresh), t(view.steam.running ? 'changes.restart_steam' : 'changes.write_changes'));
  box.replaceChildren(...[h('div.section-head', {}, link), stage, h('div.section-games', {}, ...games.map(changeRow)), restart].filter(Boolean));
}

/**
 * Above the library, like the changes in Steam: a connected controller that won't reach an
 * installed Xbox game (`controllerAlert`, decided in Rust: the driver is missing or doesn't
 * answer, or Steam Input is off for its kind), with the way to fix it.
 */
function renderControllerAlert() {
  const box = $('#controller-alert');
  const alert = settingsView && settingsView.controllerAlert;
  box.hidden = !alert;
  if (!alert) {
    box.replaceChildren();
    return;
  }
  // A controller of an unknown kind is "your controller": "your other controller" would mean a second one.
  const controller = alert.kind === 'other' ? t('alert.controller') : t(KINDS[alert.kind]);
  const text = t({
    driverMissing: 'alert.driver_missing',
    driverAsleep: 'alert.driver_asleep',
    steamInputOff: 'alert.steam_input_off',
  }[alert.reason], { controller });
  const fix = {
    driverMissing: [t('alert.get_driver'), () => openLink('driver')],
    steamInputOff: [t('alert.open_steam_settings'), () => invoke('open_steam_controller_settings').catch((error) => toast(t('toast.steam_settings_failed', { error }), 'error'))],
  }[alert.reason];
  box.replaceChildren(...[
    h('div.alert-head', {}, t('alert.title')),
    h('p.alert-text', {}, text),
    fix && h('button.restart-steam', { onclick: fix[1], disabled: busy }, fix[0]),
    h('button.alert-link', { onclick: () => openSettings('controllers') }, t('alert.open_controllers')),
  ].filter(Boolean));
  restorePadFocus();
}

/** A game in the Steam changes: what happens to it, and while a change runs, how far it is. */
function changeRow(g) {
  const info = changing && changing.rows.get(g.key);
  const waiting = view.pending.find((p) => p.key === g.key);
  const request = info ? info.action : waiting ? waiting.request : 'add';
  const state = info ? info.state : 'queued';
  const queued = { add: 'changes.queued_add', update: 'changes.queued_update', remove: 'changes.queued_remove' };
  const text = info && info.state !== 'queued' ? info.text : t(queued[request] || queued.add);
  const current = mode === 'game' && openKey === g.key;
  return h(`button.game-row.change-row${current ? '.current' : ''}`, {
    'data-key': g.key,
    'data-state': state,
    title: t('changes.row_title', { game: g.name, text }),
    'aria-current': current ? 'page' : null,
    onclick: () => openGame(g.key),
  }, rowIcon(g), h('span.change-main', {}, h('span.row-name', {}, g.name), h('small.change-text', {}, text)), h('span.change-mark', { 'aria-hidden': 'true' }));
}

function sectionGroup(s, games) {
  const isFolded = folded.has(s.id);
  const open = mode === 'tiles' && openSection === s.id;
  const foldLabel = t(isFolded ? 'sidebar.show_section' : 'sidebar.hide_section', { section: t(s.label) });
  const fold = h('button.fold', {
    title: foldLabel,
    'aria-label': foldLabel,
    'aria-expanded': String(!isFolded),
    'data-section': s.id,
    onclick: () => {
      if (isFolded) folded.delete(s.id); else folded.add(s.id);
      remember('folded', [...folded]);
      renderSidebar();
    },
  }, svgIcon(ICONS.fold));
  const link = h(`button.section-link.tone-${s.id}${open ? '.open' : ''}`, {
    title: t('sidebar.show_as_tiles', { count: games.length }),
    'aria-current': open ? 'page' : null,
    'data-section': s.id,
    onclick: () => openTiles(s.id),
  }, h('span.section-label', {}, t(s.label)), h('span.count', {}, String(games.length)), svgIcon(ICONS.tiles, 'tiles-icon'));
  return h(`div.section-group${isFolded ? '.folded' : ''}`, {},
    h('div.section-head', {}, fold, link),
    !isFolded && h('div.section-games', {}, ...games.map(gameRow)));
}

function gameRow(g) {
  const current = mode === 'game' && openKey === g.key;
  return h(`button.game-row${current ? '.current' : ''}${arrived.has(g.key) ? '.arrived' : ''}`, {
    'data-key': g.key,
    title: t('sidebar.game_title', { game: g.name, store: sourceLabel(g) }),
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
  $('#tiles-title').replaceChildren(...[t(s.label), !nothing && h('span.count', {}, String(games.length))].filter(Boolean));
  $('#tiles-hint').textContent = nothing || !s.hint ? '' : t(s.hint);
  if (nothing) {
    cards = new Map();
    selected.clear();
    $('#library').replaceChildren(emptyLibrary());
    restorePadFocus();
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
  restorePadFocus();
}

/** No games at all: what steambro reads, and how to add a game it can't find. */
function emptyLibrary() {
  const stores = view.stores.filter((st) => st.id !== 'exe');
  return h('div.empty-library', {},
    h('img.empty-cloud', { src: 'logo-glance.svg', alt: '', draggable: 'false' }),
    h('h2', {}, t('tiles.empty_title')),
    h('p', {}, t('tiles.empty_stores')),
    h('div.store-pills', {}, ...stores.map((st) => h('span.store-pill', {}, storeLogo(st.id), st.id === 'xbox' ? t('store.xbox_and_game_pass') : storeName(st.id, st.name)))),
    h('p', {}, t('tiles.empty_exe')),
    h('div.empty-actions', {},
      h('button.primary', { onclick: addExe }, svgIcon(ICONS.plus), t('sidebar.add_exe')),
      h('button', { onclick: rescan }, svgIcon(ICONS.refresh), t('tiles.look_again'))));
}

function card(g) {
  const check = h('button.check', {
    role: 'checkbox',
    'aria-label': t('tiles.select_game', { game: g.name }),
    title: t('tiles.select'),
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
    frame(g, 'cover', { fallback: h('div.placeholder', {}, h('div', {}, h('strong', {}, g.name), storeName(g.store, g.source))) }),
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
  if (g.gone) return [t('game.status_uninstalled'), 'gone'];
  if (g.outdated) return [t('game.status_old_shortcut'), 'old'];
  return [t(STATUS[g.status]), g.status];
}

// ---------------------------------------------------------------- search and tags

/** The store tags of the open section: one per store its games come from, in steambro's order. */
function storeTags() {
  const stores = new Map(sectionGames(openSection).map((g) => [g.store, storeName(g.store, g.source)]));
  const order = view.stores.map((st) => st.id);
  const rank = (id) => (order.includes(id) ? order.indexOf(id) : order.length);
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
    let text = t('tiles.none_here');
    if (query.trim()) text = t(tags.size ? 'tiles.no_match_in_stores' : 'tiles.no_match', { query: query.trim() });
    else if (tags.size) text = t('tiles.none_from_stores');
    empty.replaceChildren(h('strong', {}, t('tiles.nothing_here')), text);
  }
  renderToolbar();
}

function renderToolbar() {
  const stores = storeTags();
  for (const id of [...tags]) if (!stores.some((t) => t.id === id)) tags.delete(id);
  const pool = searched();
  // `data-tag` names the chip for the focus memory (`focusKey`); its count changes with the games.
  const chip = (tag, label, on, count, onclick, icon) =>
    h('button.tag', { 'aria-pressed': String(on), 'data-tag': tag, onclick }, icon, label, h('span.count', {}, String(count)));
  $('#tags').replaceChildren(
    chip('all', t('tiles.all_stores'), tags.size === 0, pool.length, () => { tags.clear(); applyFilter(); }),
    ...stores.map((t) => chip(t.id, t.label, tags.has(t.id), pool.filter(t.test).length, () => {
      if (tags.has(t.id)) tags.delete(t.id); else tags.add(t.id);
      applyFilter();
    }, storeLogo(t.id))));
  renderSelection();
  restorePadFocus();
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

/** Selects every game shown, or none when all of them are selected. */
function toggleAll() {
  const shown = visibleGames();
  if (shown.length && shown.every((g) => selected.has(g.key))) clearSelection();
  else selectAll();
}

/** Marks the selected cards without rebuilding them, and updates the toolbar's selection. */
function showSelection() {
  for (const [key, node] of cards) {
    node.classList.toggle('selected', selected.has(key));
    node.querySelector('.check').setAttribute('aria-checked', String(selected.has(key)));
  }
  $('#library').classList.toggle('selecting', selected.size > 0);
  renderSelection();
}

/**
 * The toolbar's selection: a checkbox for every game shown, and one button that changes the
 * selected games in Steam. Selecting starts at a card's corner, its menu, this checkbox, or Ctrl+A.
 */
function renderSelection() {
  const games = selectedGames();
  const shown = visibleGames();
  const all = shown.length > 0 && shown.every((g) => selected.has(g.key));
  const hidden = games.filter((g) => cards.get(g.key)?.hidden).length;
  const change = steamChange(games);
  const run = async () => {
    if (await changeInSteam(change)) clearSelection();
  };
  // Waiting games can only be cancelled; while a change runs, not even that.
  const waiting = changing ? [] : games.filter((g) => isChanging(g.key));
  const cancel = async () => {
    await cancelWaiting(waiting);
    clearSelection();
  };
  $('#selection').replaceChildren(...[
    h('button.select-all', {
      role: 'checkbox',
      'aria-checked': all ? 'true' : games.length ? 'mixed' : 'false',
      'aria-label': t('selection.select_all_shown'),
      title: t(all ? 'selection.select_none' : 'selection.select_all'),
      disabled: !shown.length,
      onclick: toggleAll,
    }),
    change && h(`button.primary${toneOf(change) ? `.${toneOf(change)}` : ''}`, { onclick: run, disabled: busy }, changeLabel(change)),
    waiting.length > 0 && h('button', { onclick: cancel, disabled: busy }, t('changes.cancel_count', { count: waiting.length })),
    games.length > 0 && !change && !waiting.length && h('span.selected-count', {}, t('selection.count', { count: games.length })),
    hidden > 0 && h('span.selected-hidden', {}, t('selection.hidden_count', { count: hidden })),
  ].filter(Boolean));
}

/**
 * What the toolbar's button does with the selected games in Steam, all in one change: it adds the
 * ones not in Steam, updates outdated shortcuts, and removes steambro's other shortcuts. Shortcuts
 * the user made stay. `null` when there is nothing to change.
 */
function steamChange(games) {
  // A waiting game is changed already; its selection cancels it instead.
  games = games.filter((g) => !isChanging(g.key));
  const change = {
    add: view.steam.account ? games.filter((g) => g.status === 'none') : [],
    update: games.filter((g) => g.outdated),
    remove: games.filter((g) => g.status === 'steambro' && !g.outdated),
  };
  return Object.values(change).some((list) => list.length) ? change : null;
}

/**
 * A change's button text: one phrase per kind, with its own plural, joined as a list the way the
 * language does it: `Add 2 games`, `Add 2 games and remove 5 games`, `Add 1 game, update 1
 * shortcut, and remove 3 games`.
 */
function changeLabel(change) {
  const keys = { add: 'selection.add_count', update: 'selection.update_count', remove: 'selection.remove_count' };
  const parts = Object.entries(change).filter(([, list]) => list.length).map(([verb, list]) => t(keys[verb], { count: list.length }));
  return capitalize(new Intl.ListFormat(language(), { type: 'conjunction' }).format(parts));
}

/** A change's color: red when it only removes, purple when it also adds or updates, else blue. */
function toneOf(change) {
  if (!change.remove.length) return null;
  return change.add.length || change.update.length ? 'mixed' : 'danger';
}

/**
 * What a menu can do in Steam with these games, one kind at a time: add the ones not in Steam,
 * update outdated shortcuts, remove steambro's shortcuts. `fromSelection` clears the selection
 * after a change.
 */
function steamActions(games, fromSelection = false) {
  const one = games.length === 1;
  const run = (change) => async () => {
    if ((await changeInSteam(change)) && fromSelection) clearSelection();
  };
  const waiting = changing ? [] : games.filter((g) => isChanging(g.key));
  games = games.filter((g) => !isChanging(g.key));
  const add = games.filter((g) => g.status === 'none');
  const update = games.filter((g) => g.outdated);
  const remove = games.filter((g) => g.status === 'steambro');
  const actions = [];
  if (waiting.length) {
    actions.push({ label: waiting.length === 1 && one ? t('menu.cancel_change') : t('changes.cancel_count', { count: waiting.length }), run: async () => { await cancelWaiting(waiting); if (fromSelection) clearSelection(); } });
  }
  if (add.length && view.steam.account) {
    actions.push({ label: one ? t('menu.add_to_steam') : t('menu.add_count_to_steam', { count: add.length }), run: run({ add }) });
  }
  if (update.length) actions.push({ label: one ? t('menu.update_shortcut') : t('menu.update_count', { count: update.length }), run: run({ update }) });
  if (remove.length) actions.push({ label: one ? t('menu.remove_from_steam') : t('menu.remove_count_from_steam', { count: remove.length }), danger: true, run: run({ remove }) });
  return actions;
}

/** Ignores the games, or stops ignoring them. They move to the Ignored section, or back to theirs. */
async function setIgnored(games, ignored) {
  try {
    await invoke('set_ignored', { keys: games.map((g) => g.key), ignored });
  } catch (error) {
    toast(t('toast.ignore_failed', { error }), 'error');
    return;
  }
  for (const g of games) selected.delete(g.key);
  await load();
}

// ---------------------------------------------------------------- right-click menu

/** steambro's own menu for a game, on its card or its sidebar row, in place of the browser's. */
function openCardMenu(g, x, y) {
  const group = mode === 'tiles' && selected.has(g.key) && selected.size > 1;
  const games = group ? selectedGames() : [g];
  const ignored = games.every((x) => x.ignored);
  const ignore = {
    label: group ? t(ignored ? 'menu.stop_ignoring_count' : 'menu.ignore_count', { count: games.length }) : t(ignored ? 'menu.stop_ignoring' : 'menu.ignore'),
    run: () => setIgnored(games, !ignored),
  };
  const items = group
    ? [{ label: t('selection.count', { count: games.length }), header: true }, ...steamActions(games, true), null, ignore, null, { label: t('menu.clear_selection'), run: clearSelection }]
    : [
      { label: t('menu.open'), run: () => openGame(g.key), strong: true },
      null,
      ...steamActions(games),
      !g.gone && { label: t('menu.open_folder'), run: () => invoke('open_folder', { key: g.key }) },
      null,
      ignore,
      ...(mode === 'tiles' && cards.has(g.key) ? [null, { label: t(selected.has(g.key) ? 'menu.unselect' : 'menu.select'), run: () => toggleSelected(g) }] : []),
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

/** Notes about the stores and Steam, in the content's bottom left corner. */
function renderNotices() {
  const notices = view.warnings.map((w) => h('div.notice', {}, h('strong', {}, t('alert.note')), ' ', w));
  if (!view.steam.installed) {
    notices.unshift(h('div.notice.action', {},
      h('span', {}, h('strong', {}, t('alert.steam_missing_title')), ' ', t('alert.steam_missing_text')),
      h('button', { onclick: () => openLink('steam') }, t('alert.get_steam'))));
  }
  $('#notices').replaceChildren(...notices);
  restorePadFocus();
}

function skeleton() {
  $('#tiles-title').textContent = t(ALL.label);
  $('#library').replaceChildren(...Array.from({ length: 8 }, () =>
    h('div.card', {}, h('div.frame.loading'), h('div.card-name', {}, ' '))));
}

// ---------------------------------------------------------------- game page

// The game page's tabs: [id, label, contents].
// Text keys, not texts: the constant is made before the texts load (renderDetail shows t(label)).
const TABS = [['info', 'game.tab_info', info], ['artwork', 'game.tab_artwork', artwork], ['settings', 'game.tab_settings', behavior]];

/**
 * The header, the game's name and what to do in Steam, then the tabs. A gone game has only its
 * shortcut. The way back, in the top left corner, goes to the game's section.
 */
function renderDetail() {
  const g = game(openKey);
  if (!g) return closeGame();
  const back = sectionOf(g);
  $('#back-label').textContent = t(back.label);
  $('#back').title = t('game.back_to_section', { section: t(back.label) });
  const page = $('#detail');
  page.onscroll = null;
  if (g.gone) {
    page.replaceChildren(hero(g), summary(g));
    restorePadFocus();
    return;
  }
  // The tabs stick under the window buttons' line once the page scrolls past this.
  const stickAt = () => page.querySelector('.tabs-gap').offsetTop + page.querySelector('.tabs-gap').offsetHeight - TOP_BAR;
  // Switching tabs keeps the tabs where they were when they stuck.
  const scroll = page.querySelector('.tabs-gap') ? Math.min(page.scrollTop, stickAt()) : 0;
  const warn = g.steamInputOff || g.steamOverlayOff;
  const contents = TABS.find(([id]) => id === detailTab)[2](g);
  const tabs = h('div.tabs', { role: 'tablist', 'aria-label': g.name },
      ...TABS.map(([id, label]) => h(`button.tab${id === detailTab ? '.on' : ''}`, {
        role: 'tab',
        'aria-selected': String(id === detailTab),
        'data-tab': id,
        onclick: () => { detailTab = id; renderDetail(); },
      }, t(label), id === 'settings' && warn && h('span.tab-warn', { title: t('game.tab_warn') }, '!'))));
  page.replaceChildren(hero(g), summary(g), h('div.tabs-gap'), tabs,
    h('div.tab-page', { role: 'tabpanel' }, contents || h('div.section', {}, h('p', {}, t('game.reading_settings')))));
  page.scrollTop = scroll;
  page.onscroll = () => tabs.classList.toggle('stuck', page.scrollTop >= stickAt());
  page.onscroll();
  restorePadFocus();
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

/**
 * The game's cover, name, store and status, and under them what can be done with it in Steam. On
 * the right, where it stands: when steambro added it, or why steambro leaves it alone.
 */
function summary(g) {
  const actions = [];
  let note = null;
  const waiting = view.pending.find((p) => p.key === g.key);
  if (isChanging(g.key)) {
    // What happens to the game, [now, when Steam restarts], by the waiting request.
    const notes = {
      add: ['game.now_adding', 'game.later_adding'],
      remove: ['game.now_removing', 'game.later_removing'],
      update: ['game.now_updating', 'game.later_updating'],
    };
    const keys = (waiting && notes[waiting.request]) || notes.update;
    note = t(keys[changing ? 0 : 1]);
    if (!changing) {
      actions.push(h('button.primary.restart', { onclick: restartForWaiting, disabled: busy }, svgIcon(ICONS.refresh), view.steam.running ? t('game.restart_steam') : t('game.write_changes')));
      actions.push(h('button', { onclick: () => cancelWaiting([g]), disabled: busy }, t('game.cancel_waiting')));
    }
  } else if (g.gone) {
    note = t('game.gone_note');
    actions.push(h('button.danger', { onclick: () => changeInSteam({ remove: [g] }), disabled: busy }, t('game.remove_from_steam')));
  } else if (g.status === 'none') {
    actions.push(h('button.primary', { onclick: () => changeInSteam({ add: [g] }), disabled: busy || !view.steam.account }, t('game.add_to_steam')));
  } else if (g.status === 'user') {
    note = t('game.user_note');
  } else {
    if (g.outdated) {
      note = t('game.outdated_note');
      actions.push(h('button.primary', { onclick: () => changeInSteam({ update: [g] }), disabled: busy }, t('game.update_shortcut')));
    }
    actions.push(h('button.ghost.danger', { onclick: () => changeInSteam({ remove: [g] }), disabled: busy }, t('game.remove_from_steam')));
  }
  // Y presses the first action; its text names it in the controller's hints.
  if (actions[0]) {
    actions[0].dataset.hint = actions[0].textContent.trim();
    actions[0].append(buttonMark('Y'));
  }
  const added = g.status === 'steambro' && !g.gone && !isChanging(g.key)
    && (g.added
      ? t('game.added_on', { date: new Date(g.added * 1000).toLocaleDateString(language(), { day: 'numeric', month: 'long', year: 'numeric' }) })
      : t('game.added_by_steambro'));
  return h('div.summary', {},
    frame(g, 'cover', { fallback: h('div.placeholder', {}, g.name) }),
    h('div.summary-text', {},
      h('h1', {}, g.name),
      meta(g),
      actions.length > 0 && h('div.summary-actions', {}, ...actions)),
    h('div.summary-side', {}, added && h('div.added', {}, added), note && h('div.note', {}, note)));
}

/** The Info tab: where the game is, how it starts, and where its artwork comes from. */
function info(g) {
  const row = (term, value, title) => [h('dt', {}, term), h('dd', { title }, value)];
  return h('div.section.compact', {},
    h('dl.facts', {},
      ...row(t('game.store'), h('span.store-name', {}, storeLogo(g.store), sourceLabel(g))),
      ...row(t('game.folder'), h('a', { href: '#', onclick: (e) => { e.preventDefault(); invoke('open_folder', { key: g.key }); } }, g.folder), g.folder),
      ...row(t('game.starts_with'), g.launch, g.launch),
      ...row(t('game.artwork_from'), artSourceName(g))),
    h('div.info-actions', {},
      h('button', { onclick: () => invoke('open_folder', { key: g.key }) }, t('game.open_folder')),
      h('button.ghost', { onclick: () => setIgnored([g], !g.ignored) }, g.ignored ? t('game.stop_ignoring') : t('game.ignore')),
      g.store === 'exe' && h('button.ghost', { onclick: () => forgetExe(g), disabled: busy }, t('game.forget_exe'))));
}

/** Where a game comes from, for display: `Xbox`, or `Xbox app` for an app like Netflix. */
function sourceLabel(g) {
  const store = storeName(g.store, g.source);
  return g.isApp ? t('game.store_app', { store }) : store;
}

/** A store's name for the window: the store's own (Battle.net, Xbox, ...), except "Other", which is a text. */
function storeName(id, name) {
  return id === 'exe' ? t('store.name_exe') : name;
}

/** Where a game's artwork comes from, for the texts: the store's own name, or "its exe" for an added exe. */
function artSourceName(g) {
  if (g.store === 'exe') return t('artwork.source_exe');
  // A store can take some images elsewhere, like Battle.net's games from the Microsoft Store.
  const sources = [...new Set(g.slots.map((s) => s.source).filter(Boolean))];
  return sources.length > 1 ? new Intl.ListFormat(language(), { type: 'conjunction' }).format(sources) : g.artSource;
}

/** Where the store's image of one slot comes from, for display. */
function slotSourceName(g, s) {
  return g.store === 'exe' ? t('artwork.source_exe') : s.source || g.artSource;
}

/** The store of a game, with its values, from the settings view. */
function storeOf(g) {
  return settingsView && settingsView.stores.find((st) => st.id === g.store);
}

/** A select of `options` ([value, label]); `onchange` gets the chosen value. */
function select(label, options, value, onchange) {
  return h('select', { 'aria-label': label, onchange: (e) => onchange(e.target.value) },
    ...options.map(([v, text]) => h('option', { value: v, selected: v === value }, text)));
}

/**
 * The Language setting's control: Auto (Windows' language, with a globe) and each language with
 * its flag (`flags/<code>.svg`), as buttons; the chosen one is pressed. A controller moves among
 * them like any buttons.
 */
function languagePicker(s) {
  const current = draft.language ?? 'auto';
  const auto = s.languages.find(([code]) => code === s.autoLanguage)?.[1];
  const choice = (value, icon, label) => h('button', {
    type: 'button',
    'aria-pressed': String(current === value),
    'data-key': `language:${value}`,
    title: label,
    onclick: () => { changeDraft((d) => { d.language = value === 'auto' ? null : value; }); renderSettings(); },
  }, icon, h('span', {}, label));
  return h('div.lang-picker', { role: 'group', 'aria-label': t('language.title') },
    choice('auto', svgIcon(ICONS.globe), t('language.auto', { name: auto })),
    ...s.languages.map(([code, name]) => choice(code, h('img', { src: `flags/${code}.svg`, alt: '', draggable: 'false' }), name)));
}

/** A row of the settings: a title, a line of help, and its control (a switch goes first). */
function settingRow(title, help, control) {
  const isSwitch = control.classList.contains('switch');
  return h('label.setting', {}, isSwitch && control, h('span', {}, h('strong', {}, title), h('small', {}, help)), !isSwitch && control);
}

function toggle(checked, onchange) {
  return h('input.switch', { type: 'checkbox', role: 'switch', checked, onchange: (e) => onchange(e.target.checked) });
}

// The choices of two selects, [value, text key]; `choices` gives them with their texts.
const APP_RUNNING = [['ask', 'settings.app_running_ask'], ['restart', 'settings.app_running_restart'], ['leave', 'settings.app_running_leave']];
const HAND_OVER = [['keep', 'settings.hand_over_keep'], ['close', 'settings.hand_over_close'], ['reopen', 'settings.hand_over_reopen']];

/** The options of a select from a list of [value, text key]. */
function choices(list) {
  return list.map(([v, key]) => [v, t(key)]);
}

/** Steam's overlay key, as set in Steam. */
function overlayKey() {
  return (settingsView && settingsView.steamOverlay && settingsView.steamOverlay.key) || 'Shift+Tab';
}

/** What steambro does in Steam's place while Steam can't see a game: [setting, title, help]. */
function standInParts() {
  return [
    ['bridge', t('settings.bridge_title'), t('settings.bridge_help')],
    ['layout', t('settings.layout_title'), t('settings.layout_help')],
    ['overlay', t('settings.overlay_title'), t('settings.overlay_help')],
    ['overlayKeys', t('settings.overlay_keys_title', { keys: overlayKey() }), t('settings.overlay_keys_help')],
    ['exitEndsGame', t('settings.exit_ends_game_title'), t('settings.exit_ends_game_help')],
  ];
}

/**
 * When Steam can't see a store's games (`unseen`, from the store's module), as the line before the
 * rows of what steambro does then. `what` is `store`, `game`, or `app`.
 */
function unseenLine(store, what) {
  switch (store.unseen) {
    case 'always':
      return t({ store: 'settings.unseen_always_store', game: 'settings.unseen_always_game', app: 'settings.unseen_always_app' }[what], { store: storeName(store.id, store.name) });
    case 'appOutside':
      return t('settings.unseen_app_outside', { app: store.app });
    default:
      return t({ store: 'settings.unseen_admin_store', game: 'settings.unseen_admin_game', app: 'settings.unseen_admin_app' }[what]);
  }
}

/**
 * The title and help of the rows about a store's own app, like Battle.net. `closesItself`: the
 * app's own settings close it once a game starts (`closesItself` in the settings view), so the
 * hand-over changes nothing.
 */
function appParts(app, storeId, closesItself) {
  // Epic's launcher uploads cloud saves when the game ends, so its help adds a sentence.
  const handOverHelp = closesItself
    ? t('settings.hand_over_closes_itself', { app })
    : storeId === 'epic'
      ? `${t('settings.hand_over_help', { app })} ${t('settings.hand_over_help_epic')}`
      : t('settings.hand_over_help', { app });
  return {
    appRunning: [t('settings.app_running_title', { app }), t('settings.app_running_help', { app })],
    handOver: [t('settings.hand_over_title', { app }), handOverHelp],
    closeApp: [t('settings.close_app_title', { app }), t('settings.close_app_help', { app })],
  };
}

// The help of "Start in full screen", as a text key: read it with t().
const FULL_SCREEN_HELP = 'settings.full_screen_help';

const START_WAITS = [30, 60, 90, 120, 300, 600, 1200];
const END_WAITS = [0, 3, 8, 15, 30, 60, 120];
const HAND_OVER_WAITS = [10, 20, 30, 60, 120, 300];
// Before a store that may miss a launch request is asked again (`asksAgain`).
const NUDGE_WAITS = [10, 20, 30, 60, 120];

/** The title and help of the wait before a store that may miss a launch request is asked again. */
function nudgeParts(store) {
  return [t('settings.nudge_title', { app: store.app || storeName(store.id, store.name) }), t('settings.nudge_help', { store: storeName(store.id, store.name) })];
}
// The text sizes the settings offer besides Auto, in percent (TEXT_SIZES in src/settings.rs).
const TEXT_SIZES = [90, 100, 110, 125, 140, 160];

/** A duration in seconds for display: `90 seconds`, `2 minutes`. */
function seconds(s) {
  return s >= 120 && s % 60 === 0 ? t('settings.minutes', { count: s / 60 }) : t('settings.seconds', { count: s });
}

/** The choices of a timing select, with `current` added when it isn't one of them. */
function timings(choices, current, format) {
  const all = [...new Set([...choices, current])].sort((a, b) => a - b);
  return all.map((v) => [String(v), format(v)]);
}

/** A game's Settings tab: the user's choices for the game, each shown over what it inherits. */
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
      toast(t('toast.game_settings_failed', { error }), 'error');
    }
    renderDetail();
  };
  const onOff = (on) => (on ? 'on' : 'off');
  // The first choice of every select: the store's value, which the game gets without a choice of its own.
  const inheritedChoice = (value) => ['', t('game.inherited', { value })];
  // A switch the game can turn on or off for itself. Without, it gets `g.inherited` (plan.rs).
  const inherited = (field, title, help) => {
    const first = ['', t(g.inherited[field] ? 'game.inherited_on' : 'game.inherited_off')];
    return settingRow(title, help,
      select(title, [first, ['on', t('game.on')], ['off', t('game.off')]], own[field] === null ? '' : onOff(own[field]),
        (v) => save({ [field]: v === '' ? null : v === 'on' })));
  };
  const rows = [];
  if (store.app) {
    const parts = appParts(store.app, store.id, store.closesItself);
    const appRunning = choices(APP_RUNNING);
    rows.push(settingRow(...parts.appRunning,
      select(parts.appRunning[0], [inheritedChoice(appRunning.find(([v]) => v === g.inherited.appRunning)[1]), ...appRunning], own.appRunning || '',
        (v) => save({ appRunning: v || null }))));
    const handOver = choices(HAND_OVER);
    rows.push(settingRow(...parts.handOver,
      select(parts.handOver[0], [inheritedChoice(handOver.find(([v]) => v === g.inherited.handOver)[1]), ...handOver], own.handOver || '',
        (v) => save({ handOver: v || null }))));
    rows.push(inherited('closeApp', ...parts.closeApp));
  }
  rows.push(h('p.subhead', {}, unseenLine(store, g.isApp ? 'app' : 'game')));
  for (const [field, title, help] of standInParts()) {
    const appHelp = g.isApp && field === 'bridge' ? t('game.bridge_app_help') : help;
    rows.push(inherited(field, title, appHelp));
  }
  rows.push(h('p.subhead', {}, t('game.window_head')));
  rows.push(inherited('fullScreen', t('settings.full_screen_title'), t(FULL_SCREEN_HELP)));
  rows.push(h('p.subhead', {}, t('game.waits_head')));
  const startDefault = g.inherited.startTimeoutS;
  rows.push(settingRow(t('game.start_wait_title'), t('game.start_wait_help'),
    select(t('game.start_wait_title'), [inheritedChoice(seconds(startDefault)), ...timings(START_WAITS, own.startTimeoutS ?? startDefault, seconds)],
      own.startTimeoutS === null ? '' : String(own.startTimeoutS), (v) => save({ startTimeoutS: v === '' ? null : Number(v) }))));
  if (store.asksAgain) {
    const [title, help] = nudgeParts(store);
    const ownNudge = own.nudgeS ?? null;
    rows.push(settingRow(title, help,
      select(title, [inheritedChoice(seconds(g.inherited.nudgeS)), ...timings(NUDGE_WAITS, ownNudge ?? g.inherited.nudgeS, seconds)],
        ownNudge === null ? '' : String(ownNudge), (v) => save({ nudgeS: v === '' ? null : Number(v) }))));
  }
  rows.push(settingRow(t('game.end_wait_title'), t('game.end_wait_help'),
    select(t('game.end_wait_title'), [inheritedChoice(seconds(g.inherited.endGraceS)), ...timings(END_WAITS, own.endGraceS ?? g.inherited.endGraceS, seconds)],
      own.endGraceS === null ? '' : String(own.endGraceS), (v) => save({ endGraceS: v === '' ? null : Number(v) }))));
  if (store.app) {
    const title = t('game.hand_over_wait_title', { app: store.app });
    rows.push(settingRow(title, t('game.hand_over_wait_help', { app: store.app }),
      select(title, [inheritedChoice(seconds(g.inherited.handOverS)), ...timings(HAND_OVER_WAITS, own.handOverS ?? g.inherited.handOverS, seconds)],
        own.handOverS === null ? '' : String(own.handOverS), (v) => save({ handOverS: v === '' ? null : Number(v) }))));
  }
  return h('div.section.compact', {},
    h('p', {}, t('game.settings_intro')),
    ...rows,
    g.steamInputOff && checkRow({
      state: 'warn',
      name: t('game.steam_input_off_title'),
      text: t('game.steam_input_off_help'),
    }),
    g.steamOverlayOff && checkRow({
      state: 'warn',
      name: t('game.overlay_off_title'),
      text: t('game.overlay_off_help'),
    }));
}

function artwork(g) {
  const editable = g.status !== 'user';
  const intro = {
    none: t('artwork.intro_none', { source: artSourceName(g) }),
    steambro: t('artwork.intro_steambro'),
    user: t('artwork.intro_user'),
  }[g.status];
  return h('div.section', {},
    h('p', {}, intro),
    h('div.slots', {}, ...g.slots.map((s) => slotTile(g, s, editable))));
}

// The name of each artwork slot (Slot in src/art/mod.rs), as a text key.
const SLOT_LABELS = {
  cover: 'artwork.slot_cover',
  hero: 'artwork.slot_hero',
  logo: 'artwork.slot_logo',
  wide: 'artwork.slot_wide',
  icon: 'artwork.slot_icon',
};

/** A slot's name in the user's language, like `cover`; a slot the page doesn't know keeps Rust's label. */
function slotLabel(s) {
  return SLOT_LABELS[s.slot] ? t(SLOT_LABELS[s.slot]) : s.label;
}

function slotState(g, s) {
  if (g.status === 'user') return s.inSteam ? [t('artwork.in_steam'), 'ok'] : [t('artwork.steam_default'), ''];
  switch (s.placement) {
    case 'written': return [s.own ? t('artwork.own_in_steam') : t('artwork.in_steam'), 'ok'];
    case 'kept': return [t('artwork.kept'), ''];
    case 'pending': return [t('artwork.pending'), 'warn'];
    case 'unavailable': return [t('artwork.source_has_none', { source: slotSourceName(g, s) }), ''];
    default: return [s.own ? t('artwork.own') : t('artwork.from_source', { source: slotSourceName(g, s) }), ''];
  }
}

function slotTile(g, s, editable) {
  const [text, tone] = slotState(g, s);
  const state = h(`span.slot-state${tone ? '.' + tone : ''}`, {}, text);
  let hasImage = true;
  const picture = frame(g, s.slot, {
    fallback: h('div.placeholder', {}, s.slot === 'logo' ? t('artwork.no_logo') : t('artwork.no_image')),
    onMissing: () => {
      hasImage = false;
      if (!s.placement && !s.own) state.textContent = t('artwork.source_has_none', { source: slotSourceName(g, s) });
      const adjust = tile.querySelector('[data-action="adjust"]');
      if (adjust) adjust.remove();
    },
  });
  // While a change to Steam runs, edits wait, so they are off.
  const actions = editable ? h('div.slot-actions', {},
    s.shape && !(s.placement === 'kept') && h('button', { 'data-action': 'adjust', disabled: busy, onclick: () => hasImage && openCropper(g, s) }, t('artwork.adjust')),
    h('button', { disabled: busy, onclick: () => pickFile(g, s) }, t('artwork.replace')),
    (s.own || s.crop || s.placement === 'kept') && h('button.ghost', { disabled: busy, onclick: () => resetSlot(g, s) }, s.placement === 'kept' ? t('artwork.use_steambros') : t('artwork.reset')),
  ) : null;
  // On: steambro takes the store's newer image each time the window opens. Steam's own image
  // (kept) and the user's own image don't follow the store, so the switch shows off for them. A
  // slot the store never fills, like an exe's cover, has no switch.
  const source = slotSourceName(g, s);
  const follow = editable && s.fillable && h('label.switch-line', { title: t('artwork.follow_help', { source }) },
    h('input.switch', {
      type: 'checkbox', role: 'switch', checked: s.follows && s.placement !== 'kept', disabled: busy,
      'data-key': `${s.slot}:follow`, onchange: (e) => setFollow(g, s, e.target),
    }),
    t('artwork.follow', { source }));
  const tile = h('div.slot', { 'data-slot': s.slot },
    h('div.slot-head', {}, h('strong', {}, slotLabel(s)), state),
    picture,
    actions,
    follow);
  if (editable) {
    tile.addEventListener('dragover', (e) => { e.preventDefault(); tile.classList.add('drop'); });
    tile.addEventListener('dragleave', () => tile.classList.remove('drop'));
    tile.addEventListener('drop', (e) => {
      e.preventDefault();
      tile.classList.remove('drop');
      const file = e.dataTransfer.files[0];
      if (file && !busy) replaceWith(g, s, file);
    });
  }
  return tile;
}

// ---------------------------------------------------------------- artwork edits

/** Edits overwrite a slot where Steam had its own image, so ask first. */
async function confirmOverwrite(g, s) {
  if (s.placement !== 'kept') return true;
  return ask({
    title: t('artwork.replace_steams_title', { slot: slotLabel(s) }),
    text: t('artwork.replace_steams_text'),
    confirm: t('artwork.replace_steams_confirm'),
  });
}

/** Windows' file dialog, opened beside the slot's image (pick_art in src/app/commands.rs). */
async function pickFile(g, s) {
  if (!(await confirmOverwrite(g, s))) return;
  try {
    const picked = await invoke('pick_art', { key: g.key, slot: s.slot, title: t('artwork.pick_title', { slot: slotLabel(s) }) });
    if (!picked) return;
    toast(t('toast.art_replaced', { slot: capitalize(slotLabel(s)) }), 'ok');
  } catch (error) {
    toast(String(error), 'error');
  }
  artVersion = Date.now();
  await load();
}

/** Turns the slot's updates from the store on or off; on puts the store's image back. */
async function setFollow(g, s, input) {
  const on = input.checked;
  if (on && !(await confirmOverwrite(g, s))) {
    input.checked = false;
    return;
  }
  const vars = { slot: capitalize(slotLabel(s)), source: slotSourceName(g, s) };
  await change(() => invoke('set_art_follow', { key: g.key, slot: s.slot, follow: on }), t(on ? 'toast.art_follows' : 'toast.art_stays', vars));
}

async function replaceWith(g, s, file) {
  if (!/^image\/(png|jpeg|webp)$/.test(file.type)) return toast(t('toast.art_bad_type'), 'error');
  if (!(await confirmOverwrite(g, s))) return;
  const bytes = new Uint8Array(await file.arrayBuffer());
  await change(() => invoke('replace_art', bytes, { headers: { 'x-game': encodeURIComponent(g.key), 'x-slot': s.slot } }),
    t('toast.art_replaced', { slot: capitalize(slotLabel(s)) }));
}

/**
 * Reset goes back to the store's image. On a slot where Steam kept its own image, the button puts
 * steambro's image there as it is now, the user's own included (set_crop with the same crop).
 */
async function resetSlot(g, s) {
  if (!(await confirmOverwrite(g, s))) return;
  const vars = { slot: capitalize(slotLabel(s)) };
  if (s.placement === 'kept') return change(() => invoke('set_crop', { key: g.key, slot: s.slot, crop: s.crop ?? null }), t('toast.art_updated', vars));
  await change(() => invoke('reset_art', { key: g.key, slot: s.slot }), t('toast.art_reset', vars));
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
    await change(() => invoke('set_crop', { key: g.key, slot: s.slot, crop: same ? null : crop }), t('toast.art_updated', { slot: capitalize(slotLabel(s)) }));
  };
  const backToDefault = () => {
    if (!crop) return;
    crop = { ...standard };
    zoom.value = String(full.w / standard.w);
    place();
  };
  // Moves the frame by a share of its own size; the zoom by a factor.
  const nudge = (dx, dy) => {
    if (!crop) return;
    crop.x += dx * crop.w;
    crop.y += dy * crop.h;
    clamp();
    place();
  };
  const zoomBy = (factor) => {
    if (!crop) return;
    const z = Math.min(4, Math.max(1, Number(zoom.value) * factor));
    zoom.value = String(z);
    setZoom(z);
  };
  const body = h('div.dialog-body', {},
    h('h3', {}, t('crop.title', { slot: slotLabel(s) })),
    h('p', {}, t('crop.help')),
    h('div.cropper', {}, stage,
      h('div.crop-side', {},
        h('label', {}, t('crop.zoom'), zoom),
        h('label', {}, t('crop.preview'), preview))));
  dialog.replaceChildren(body,
    h('div.dialog-actions', {},
      h('button.ghost.start', { onclick: backToDefault }, buttonMark('Y'), t('crop.default')),
      h('button', { onclick: () => dialog.close() }, buttonMark('B', true), t('crop.cancel')),
      h('button.primary', { onclick: save }, t('crop.save'), buttonMark('A', true))));
  dialog.showModal();
  // showModal focuses the zoom slider, which would take the arrow keys; the dialog takes the focus.
  dialog.tabIndex = -1;
  dialog.focus();
  cropController(dialog, body, { nudge, zoomBy, backToDefault, save });
}

/**
 * A controller and the arrow keys in the crop dialog, while it shows `body`: the D-pad, the left
 * stick, or the arrows move the frame (held, it keeps moving), LB and RB zoom out and in, A or
 * Enter saves, B or Escape cancels, and Y goes back to the default. The window's own poll rests
 * while the dialog is open, so this one reads the controllers. A Steam Controller sends the arrow
 * keys and Enter through Steam's desktop layout.
 */
function cropController(dialog, body, { nudge, zoomBy, backToDefault, save }) {
  const open = () => dialog.open && dialog.contains(body);
  dialog.classList.toggle('pad-hints', padSeen);
  const keys = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] };
  const onKey = (e) => {
    if (!open()) return dialog.removeEventListener('keydown', onKey);
    const step = keys[e.key];
    if (step && !(e.target instanceof HTMLInputElement)) {
      e.preventDefault();
      nudge(step[0] * 0.04, step[1] * 0.04);
    } else if (e.key === 'Enter' && !e.repeat && !(e.target instanceof HTMLButtonElement)) {
      e.preventDefault();
      save();
    }
  };
  dialog.addEventListener('keydown', onKey);
  // Held buttons count once; the stick and the D-pad move by how long they are held.
  const held = new Set();
  let primed = false;
  let last = performance.now();
  const frame = (now) => {
    if (!open()) return;
    requestAnimationFrame(frame);
    const dt = Math.min(0.05, (now - last) / 1000);
    last = now;
    for (const pad of standardPads()) {
      if (!pad) continue;
      const pressed = (i) => Boolean(pad.buttons[i] && pad.buttons[i].pressed);
      let [x = 0, y = 0] = pad.axes;
      if (Math.abs(x) < 0.2) x = 0;
      if (Math.abs(y) < 0.2) y = 0;
      x += (pressed(15) ? 1 : 0) - (pressed(14) ? 1 : 0);
      y += (pressed(13) ? 1 : 0) - (pressed(12) ? 1 : 0);
      // Half the frame's size a second at full tilt.
      if (x || y) nudge(x * 0.5 * dt, y * 0.5 * dt);
      if (pressed(4)) zoomBy(1 - 0.8 * dt);
      if (pressed(5)) zoomBy(1 + 0.8 * dt);
      for (const [i, name, act] of [[0, 'a', save], [1, 'b', () => dialog.close()], [3, 'y', backToDefault]]) {
        const id = `${pad.index}:${name}`;
        if (!pressed(i)) {
          held.delete(id);
          continue;
        }
        if (held.has(id)) continue;
        held.add(id);
        if (primed && !keyEchoed(name)) {
          if (!padSeen) {
            padSeen = true;
            dialog.classList.add('pad-hints');
          }
          act();
        }
      }
    }
    primed = true;
  };
  requestAnimationFrame(frame);
}

// ---------------------------------------------------------------- adding games

function accountName(account) {
  return account.name || t('dialog.account_number', { id: account.id });
}

// What the window says when it adds games, updates shortcuts that start their games an older way,
// removes steambro's shortcuts, or does more than one of these at once (`mixed`): the question of
// accounts, its button, and the toast at the end. All of them go through `change_steam`. The texts
// come from ui/i18n (changes.*) when they show, so each is a function that calls t() then:
// `failed` is the toast when the whole change failed with an error, `none` when no game went through.
const WORDS = {
  add: {
    ask: (games) => (games.length > 1 ? t('changes.add_ask_many', { count: games.length }) : t('changes.add_ask', { game: games[0].name })),
    confirm: () => t('changes.add_confirm'),
    done: (n, games) => (games.length === 1 ? t('changes.add_done', { game: games[0].name }) : t('changes.add_done_many', { count: n })),
    partly: (n, of) => t('changes.add_partly', { done: n, count: of }),
    failed: (error) => t('changes.add_failed', { error }),
    none: () => t('changes.add_none'),
  },
  update: {
    ask: (games) => (games.length > 1 ? t('changes.update_ask_many', { count: games.length }) : t('changes.update_ask', { game: games[0].name })),
    confirm: () => t('changes.update_confirm'),
    done: (n, games) => (games.length === 1 ? t('changes.update_done', { game: games[0].name }) : t('changes.update_done_many', { count: n })),
    partly: (n, of) => t('changes.update_partly', { done: n, count: of }),
    failed: (error) => t('changes.update_failed', { error }),
    none: () => t('changes.update_none'),
  },
  remove: {
    ask: (games) => (games.length > 1 ? t('changes.remove_ask_many', { count: games.length }) : t('changes.remove_ask', { game: games[0].name })),
    confirm: () => t('changes.remove_confirm'),
    done: (n, games) => (games.length === 1 ? t('changes.remove_done', { game: games[0].name }) : t('changes.remove_done_many', { count: n })),
    partly: (n, of) => t('changes.remove_partly', { done: n, count: of }),
    failed: (error) => t('changes.remove_failed', { error }),
    none: () => t('changes.remove_none'),
  },
  mixed: {
    ask: (games, change) => t('changes.mixed_ask', { change: changeLabel(change) }),
    confirm: () => t('changes.mixed_confirm'),
    done: (n) => t('changes.mixed_done', { count: n }),
    partly: (n, of) => t('changes.mixed_partly', { done: n, count: of }),
    failed: (error) => t('changes.mixed_failed', { error }),
    none: () => t('changes.mixed_none'),
  },
};

// A game's line in the question's list when the change does more than one thing to the games.
const LISTED = {
  add: (game) => t('changes.list_add', { game }),
  update: (game) => t('changes.list_update', { game }),
  remove: (game) => t('changes.list_remove', { game }),
};

/** The games of the changes that wait for Steam to restart, other than `except`. */
function waitingGames(except = []) {
  return view.pending.filter((p) => !except.some((g) => g.key === p.key)).map((p) => game(p.key)).filter(Boolean);
}

/**
 * Changes games in Steam: adds the games in `add`, updates the outdated shortcuts in `update`, and
 * removes steambro's shortcuts in `remove`. With Steam closed, steambro writes the change at once.
 * While Steam runs, the games wait for its restart right away (`view.pending`, the section above
 * the library), and the only question is whether to restart Steam now or later; a waiting change
 * can be cancelled there. With more than one Steam account, the accounts come first. Resolves to
 * true when the change ran or waits, false when the user cancelled the choice of accounts.
 */
async function changeInSteam({ add = [], update = [], remove = [] }) {
  const change = { add, update, remove };
  const games = [...add, ...update, ...remove];
  if (busy || games.length === 0) return false;
  const kinds = Object.keys(change).filter((kind) => change[kind].length);
  const words = WORDS[kinds.length === 1 ? kinds[0] : 'mixed'];
  const { running, account, accounts } = view.steam;
  let allAccounts = false;
  if (accounts.length > 1) {
    const answer = await ask({
      title: words.ask(games, change),
      text: t('dialog.accounts_text'),
      list: games.length > 1 ? kinds.flatMap((kind) => change[kind].map((g) => (kinds.length > 1 ? LISTED[kind](g.name) : g.name))) : null,
      choices: [
        { value: 'current', label: t('dialog.only_account', { account: accountName(account) }), detail: t('dialog.last_account') },
        { value: 'all', label: t('dialog.all_accounts'), detail: accounts.map(accountName).join(', ') },
      ],
      confirm: words.confirm(false),
      tone: toneOf(change),
    });
    if (!answer) return false;
    allAccounts = answer === 'all';
  }
  if (!running) return runChange({ games, add, update, remove, allAccounts, later: false, words });
  // The games wait for Steam's restart while the question shows.
  const queued = runChange({ games, add, update, remove, allAccounts, later: true, words, quiet: true });
  const now = await ask({
    title: t('dialog.restart_title'),
    text: games.length === 1 ? t('dialog.restart_text', { game: games[0].name }) : t('dialog.restart_text_many', { count: games.length }),
    cancel: t('dialog.restart_later'),
    confirm: t('dialog.restart_now'),
  });
  await queued;
  if (now) await restartForWaiting();
  return true;
}

/** Restarts Steam for the changes that wait for it, or writes them at once while Steam is closed. */
async function restartForWaiting() {
  const games = waitingGames();
  if (busy || !games.length) return;
  await runChange({ games: [], add: [], update: [], remove: [], allAccounts: false, later: false, words: WORDS.mixed, waiting: games });
}

/**
 * Cancels the waiting changes of these games: for each, the opposite request with `later`, which
 * replaces the waiting one and queues nothing. They go back to their sections.
 */
async function cancelWaiting(games) {
  const requests = games.map((g) => view.pending.find((p) => p.key === g.key)).filter(Boolean);
  if (busy || !requests.length) return;
  try {
    await invoke('change_steam', {
      add: requests.filter((p) => p.request === 'remove').map((p) => p.key),
      remove: requests.filter((p) => p.request === 'add').map((p) => p.key),
      allAccounts: true,
      restartSteam: false,
      later: true,
    });
  } catch (error) {
    toast(t('toast.cancel_failed', { error }), 'error');
  }
  await load();
}

/**
 * Runs a change that the user confirmed. Its games show above the library while it runs
 * (`changing`, renderSteamChanges), each with how far it is, and move to their new section as
 * soon as they're done; a toast says how it ended.
 */
async function runChange({ games, add, update, remove, allAccounts, later, words, waiting = null, quiet = false }) {
  const { running } = view.steam;
  // A change that writes Steam's shortcuts writes the waiting ones too, so their rows show.
  const extra = later ? [] : waiting || waitingGames(games);
  const actions = new Map([
    ...add.map((g) => [g.key, 'add']),
    ...update.map((g) => [g.key, 'update']),
    ...remove.map((g) => [g.key, 'remove']),
    ...extra.map((g) => [g.key, (view.pending.find((p) => p.key === g.key) || {}).request || 'add']),
  ]);
  changing = {
    later,
    stage: add.length ? t('changes.stage_artwork') : '',
    done: 0,
    queued: 0,
    rows: new Map([...games, ...extra].map((g) => [g.key, { g, action: actions.get(g.key), state: 'waiting', text: t('changes.row_waiting') }])),
  };
  busy = true;
  render();
  let outcome = null;
  let failure = null;
  try {
    // Updating is adding to Steam: it updates a game's outdated shortcut in place.
    const keys = (list) => list.map((g) => g.key);
    outcome = await invoke('change_steam', { add: keys([...add, ...update]), remove: keys(remove), allAccounts, restartSteam: running && !later, later });
  } catch (error) {
    failure = String(error);
  }
  const { done, queued } = changing;
  const failed = [...changing.rows.values()].filter((row) => row.state === 'failed');
  changing = null;
  busy = false;
  if (outcome && outcome.needsRestart) {
    // Steam started after the window last looked: ask again, with the restart.
    view.steam.running = true;
    render();
    return games.length ? changeInSteam({ add, update, remove }) : restartForWaiting();
  }
  const all = games.length + extra.length;
  if (failure) toast(words.failed(failure), 'error');
  else if (failed.length) {
    // The outcome, then one sentence per game that failed, each with the reason from Rust.
    const reasons = failed.map((row) => t('changes.row_failed', { game: row.g.name, error: row.text }));
    toast([done ? words.partly(done, all) : words.none(), ...reasons].join(' '), 'error');
  } else if (done) {
    const ended = words.done(done, [...games, ...extra]);
    toast(outcome.steamStarted ? `${ended} ${t('changes.steam_restarting')}` : ended, 'ok');
  } else if (queued) { if (!quiet) toast(t('toast.games_wait', { count: queued }), 'ok'); }
  else toast(t('toast.nothing_to_change'));
  artVersion = Date.now();
  await load();
  return true;
}

/** A game's step in the change that runs (`progress-game`, see change_steam in src/app/commands.rs). */
function changeStep(key, step) {
  const row = changing && changing.rows.get(key);
  if (!row) return;
  const set = (state, text) => { row.state = state; row.text = text; };
  // What the row says when its change waits for Steam's restart, by what the change does to the game.
  const queued = {
    add: () => t('changes.row_queued_add'),
    update: () => t('changes.row_queued_update'),
    remove: () => t('changes.row_queued_remove'),
  };
  switch (step.kind) {
    case 'artwork': set('working', t('changes.row_artwork', { number: step.number, total: step.of })); break;
    case 'ready': set('ready', t('changes.row_artwork_ready')); break;
    case 'queued': set('queued', (queued[row.action] || queued.add)()); changing.queued += 1; break;
    case 'added': case 'updated': arrive(key, 'steambro'); break;
    case 'removed': arrive(key, 'none'); break;
    case 'alreadyInSteam': case 'notInSteam': changing.rows.delete(key); break;
    case 'failed': set('failed', step.error); break;
    default: return;
  }
  renderSidebar();
}

/** What Steam does in the change that runs (`progress-steam`). */
function changeStage(action) {
  if (!changing) return;
  const stages = {
    closing: () => t('changes.stage_closing'),
    writing: () => t('changes.stage_writing'),
    starting: () => t('changes.stage_starting'),
  };
  changing.stage = stages[action] ? stages[action]() : '';
  if (action === 'writing') {
    for (const row of changing.rows.values()) if (row.state !== 'failed') { row.state = 'working'; row.text = t('changes.row_writing'); }
  }
  renderSidebar();
}

/** A game is done: it leaves the changes for its new section, where its row lights up for a moment. */
function arrive(key, status) {
  changing.rows.delete(key);
  changing.done += 1;
  const g = game(key);
  if (g && g.gone && status === 'none') {
    everything = everything.filter((x) => x !== g);
  } else if (g) {
    g.status = status;
    g.outdated = false;
  }
  arrived.add(key);
  setTimeout(() => { arrived.delete(key); if (view) renderSidebar(); }, 2400);
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

// The cloud at the top of the sidebar, for fun: it squeezes its eyes while the mouse is over it,
// and glances away while it is pressed. The tray icon keeps the app's mood.
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
    $('#wc-max').title = maximized ? t('window.restore') : t('window.maximize');
    $('#wc-max').setAttribute('aria-label', $('#wc-max').title);
  };
  win.onResized(sync);
  sync();
}

// ---------------------------------------------------------------- settings

// What the settings window shows (settings_view in src/app/commands.rs). The controller alert in
// the sidebar and a game's Settings tab use it too.
let settingsView = null;
// The settings window's page: 'general', 'controllers', a store's ID, or 'about'.
let settingsPage = 'general';
// The settings as the window changes them, until Save sends them all at once (save_settings in
// src/app/commands.rs). `savedDraft` is how they were, so the window knows what changed.
let draft = null;
let savedDraft = '';
// The page the settings window showed last, to keep its scroll when it shows it again.
let shownPage = null;

// The kinds of controllers, as keys of their names in ui/i18n (controllers.kind_*): a module-level
// constant is made before the texts load, so a renderer shows `t(KINDS[kind])`.
const KINDS = {
  xbox: 'controllers.kind_xbox',
  steam: 'controllers.kind_steam',
  playstation: 'controllers.kind_playstation',
  nintendo: 'controllers.kind_nintendo',
  other: 'controllers.kind_other',
};

async function loadSettings() {
  try {
    settingsView = await invoke('settings_view');
  } catch (error) {
    if ($('#settings').open) showSettingsError(t('settings.read_failed', { error }));
    return;
  }
  if (view && mode === 'game') renderDetail();
  renderControllerAlert();
  if ($('#settings').open) {
    // Changes the user made stay; without any, the window shows what was just read.
    if (!changed()) startDraft();
    renderSettings();
  }
}

/**
 * One line of the checks: `state` is ok, warn, bad or idle; `action` is [label, onclick]. `key`
 * tells the action's button apart from the same button on other rows, so the focus finds it again
 * after a render (`focusKey`).
 */
function checkRow({ state, name, text, action, link, key }) {
  const mark = { ok: '\u2713', warn: '!', bad: '\u2715', idle: '\u2013' }[state];
  return h(`div.check-row.${state}`, {},
    h('span.mark', { 'aria-hidden': 'true' }, mark),
    h('span', {}, h('strong', {}, name), h('small', {}, text)),
    action && h('button', { onclick: action[1], disabled: busy, 'data-key': key }, action[0]),
    link && h('a.check-link', { href: '#', 'data-key': key, onclick: (e) => { e.preventDefault(); link[1](); } }, link[0]));
}

/**
 * The driver's line of the checks: installed, installed but not answering, or not installed. Its
 * page on steambro.app is always one click away: a button while the driver is missing, a link
 * otherwise.
 */
function driverCheck(d) {
  const name = d.version ? t('controllers.driver_with_version', { version: d.version }) : t('controllers.driver');
  const page = [t('controllers.driver_page'), () => openLink('driver')];
  if (d.available) return { state: 'ok', name, text: t('controllers.driver_installed'), link: page };
  if (d.installed) return { state: 'warn', name: t('controllers.driver'), text: t('controllers.driver_not_answering'), link: page };
  return {
    state: 'bad',
    name: t('controllers.driver'),
    text: t('controllers.driver_missing'),
    action: [t('controllers.driver_get'), () => openLink('driver')],
  };
}

/** A controller's name for the table and the questions: the model's when steambro knows it, else its kind's. */
function controllerName(c) {
  // KINDS holds keys, or English while the texts load: t() gives back what it can't find.
  return c.name || capitalize(t(KINDS[c.kind]));
}

/**
 * The table of controllers (`controllers`, built in Rust): one row per controller the bridge
 * knows and per kind connected now or used with Steam before. Columns: the controller with its
 * ID, Steam Input for its kind, and the virtual controller it drives, with the virtual
 * controller's model and the "Steam ignores" switch (`driver.hiddenFromSteam`). The virtual
 * controllers are one model to Steam, so every row's switch is the same switch. A row with a
 * virtual controller has "Forget".
 */
function controllerTable(s) {
  const steamInput = (c) => {
    if (c.steamInput === true) return t('controllers.steam_input_on');
    if (c.steamInput === false) {
      return h('button.link.amber', { onclick: () => invoke('open_steam_controller_settings').catch((error) => toast(t('toast.steam_settings_failed', { error }), 'error')), 'data-key': `${c.id}:steam-input` }, t('controllers.steam_input_off'));
    }
    return h('span.faint', {}, s.steamOverlay ? t('controllers.steam_input_default') : '—');
  };
  const ignores = (c) => s.driver.hiddenFromSteam !== null && h('label.switch-line', {},
    h('input.switch', { type: 'checkbox', role: 'switch', checked: s.driver.hiddenFromSteam, disabled: busy, 'aria-label': t('controllers.ignores_aria', { number: c.number }), 'data-key': `${c.id}:hidden`, onchange: (e) => setVirtualControllersHidden(e.target.checked) }),
    t('controllers.ignores'));
  const pad = (c) => {
    if (!c.bridged) return [h('span.faint', {}, '—'), h('small', {}, t('controllers.pad_direct'))];
    if (c.number !== null) {
      return [
        t('controllers.pad_name', { number: c.number }),
        h('small.mono', {}, `${s.driver.padModel} · ${c.last ? t('controllers.pad_last_player') : t('controllers.pad_keeps_number')}`),
        ignores(c),
      ];
    }
    if (!s.driver.available) return [h('span.faint', {}, '—'), h('small', {}, t('controllers.pad_needs_driver'))];
    return [h('span.faint', {}, t('controllers.pad_none')), h('small', {}, t('controllers.pad_made_later'))];
  };
  const row = (c) => h('tr', {},
    h('td', {},
      h('span.ctl-name', {}, h(`span.dot${c.connected ? '.on' : ''}`, { 'aria-hidden': 'true' }), controllerName(c)),
      h('small', {}, c.connected ? t('controllers.connected') : t('controllers.not_connected')),
      c.model && h('small.mono', {}, c.id.includes('#') ? c.id : c.model)),
    h('td', {}, steamInput(c)),
    h('td', {}, ...pad(c)),
    h('td.center', {}, c.number !== null && h('button.small', { onclick: () => forgetVirtualController(c), disabled: busy, 'data-key': c.id }, t('controllers.forget'))));
  return h('table.controller-table', {},
    h('thead', {}, h('tr', {}, h('th', {}, t('controllers.column_controller')), h('th', {}, t('controllers.column_steam_input')), h('th', {}, t('controllers.column_virtual')), h('th', {}))),
    h('tbody', {}, ...s.controllers.map(row)));
}

/**
 * Hides steambro's virtual controllers from Steam, or shows them again
 * (`hide_virtual_controllers`). Steam restarts for it when it runs, so it asks first then; closed,
 * it changes at once. steambro hides them itself whenever it changes Steam, so the switch is on
 * unless the user turned it off, which Rust remembers.
 */
async function setVirtualControllersHidden(hidden) {
  if (busy) return;
  if (view.steam.running) {
    const ok = await ask({
      title: hidden ? t('dialog.hide_virtual_title') : t('dialog.show_virtual_title'),
      text: `${hidden ? t('dialog.hide_virtual_text') : t('dialog.show_virtual_text')} ${t('dialog.steam_restarts')}`,
      confirm: t('dialog.restart_steam_and_change'),
    });
    if (!ok) return renderSettings();
  }
  busy = true;
  renderSettings();
  try {
    await invoke('hide_virtual_controllers', { hidden });
  } catch (error) {
    toast(t('toast.virtual_controllers_failed', { error }), 'error');
  }
  busy = false;
  await loadSettings();
}

/** Takes a controller out of steambro's virtual controllers (`forget_virtual_controllers`), so its number is free. */
async function forgetVirtualController(c) {
  if (busy) return;
  const ok = await ask({
    title: t('dialog.forget_controller_title'),
    text: t('dialog.forget_controller_text'),
    list: [t('controllers.name_with_id', { name: controllerName(c), id: c.id })],
    confirm: t('controllers.forget'),
  });
  if (!ok) return;
  busy = true;
  renderSettings();
  try {
    await invoke('forget_virtual_controllers', { ids: [c.id] });
  } catch (error) {
    toast(t('toast.forget_controller_failed', { error }), 'error');
  }
  busy = false;
  await loadSettings();
}

/** Opens the settings window at `page`, over everything, which takes no clicks meanwhile. */
function openSettings(page = settingsPage) {
  settingsPage = page;
  const dialog = $('#settings');
  if (!dialog.open) {
    draft = null;
    dialog.showModal();
  }
  renderSettings();
  loadSettings();
  padLand('#settings .page-link[aria-current="page"]');
}

/** The draft starts as the saved settings. */
function startDraft() {
  const s = settingsView.settings;
  draft = {
    newGameNotices: s.newGameNotices,
    textSize: s.textSize ?? null,
    language: s.language ?? null,
    launch: { ...s.launch },
    stores: Object.fromEntries(settingsView.stores.map((st) => [st.id, { ...st.settings }])),
  };
  savedDraft = JSON.stringify(draft);
}

function changed() {
  return draft !== null && JSON.stringify(draft) !== savedDraft;
}

/** Changes the draft; Save is on while anything changed. */
function changeDraft(change) {
  change(draft);
  const save = $('#settings .save');
  if (save) save.disabled = !changed();
  // Y saves while Save is on.
  renderPadHints();
}

function showSettingsError(message) {
  const line = $('#settings .bar-error');
  if (line) line.textContent = message;
}

/** Closes the settings window. Changes not saved go, after a question. */
async function closeSettings() {
  if (changed() && !(await ask({ title: t('dialog.discard_title'), text: t('dialog.discard_text'), cancel: t('dialog.keep_editing'), confirm: t('dialog.discard'), tone: 'danger' }))) return;
  draft = null;
  $('#settings').close();
}

/**
 * Saves every change at once. Launches and the tray app read the settings each time, so no change
 * needs a restart.
 */
async function saveSettings() {
  const languageChanged = draft.language !== (settingsView.settings.language ?? null);
  try {
    await invoke('save_settings', { change: draft });
  } catch (error) {
    showSettingsError(t('settings.save_failed', { error }));
    return;
  }
  draft = null;
  $('#settings').close();
  if (languageChanged) {
    // Everything on the page reads the texts again as it renders.
    await loadStrings().catch(() => {});
    translatePage();
    render();
  }
  toast(t('settings.saved'), 'ok');
  loadSettings();
  // What a game inherits follows its store's settings.
  load();
}

/**
 * The settings window: its pages on the left (General, Controllers, each store, and About at the
 * bottom), the open page, and Cancel and Save at the bottom. Its empty parts move the main window.
 */
function renderSettings() {
  const dialog = $('#settings');
  if (!dialog.open) return;
  const s = settingsView;
  if (s && !draft) startDraft();
  const keep = settingsPage === shownPage ? dialog.querySelector('.settings-page')?.scrollTop : 0;
  shownPage = settingsPage;
  const link = (id, label, icon) => h('button.page-link', {
    'aria-current': id === settingsPage ? 'page' : null,
    'data-page': id,
    onclick: () => { settingsPage = id; renderSettings(); },
  }, icon, h('span', {}, label));
  const stores = s ? s.stores : view ? view.stores : [];
  const page = h('div.settings-page', {}, ...(s ? settingsContent(s) : [h('p', {}, t('settings.reading'))]));
  dialog.replaceChildren(
    h('nav.settings-nav', { 'aria-label': t('settings.pages_aria'), 'data-drag': true },
      h('h2', {}, t('settings.title')),
      h('div.page-links', {},
        link('general', t('settings.general'), svgIcon(ICONS.sliders)),
        link('controllers', t('settings.controllers'), svgIcon(ICONS.gamepad)),
        ...stores.map((st) => link(st.id, storeName(st.id, st.name), storeLogo(st.id)))),
      // Outside the Store package only: the button opens steambro's Store listing.
      s && !s.packaged && h('div.nav-support', {},
        h('button', { onclick: () => openLink('store'), 'data-key': 'support' }, svgIcon(ICONS.heart), h('span', {}, t('settings.support')))),
      h('div.nav-foot', {},
        link('about', t('settings.about'), h('img', { src: 'logo-wink.svg', alt: '', draggable: 'false' })))),
    h('div.settings-main', {},
      page,
      h('div.settings-bar', {},
        h('span.bar-error', { role: 'alert' }),
        h('button', { onclick: closeSettings }, buttonMark('B', true), t('settings.cancel')),
        h('button.primary.save', { onclick: saveSettings, disabled: !changed() }, t('settings.save'), buttonMark('Y')))));
  page.scrollTop = keep || 0;
  restorePadFocus();
  renderPadHints();
}

function settingsContent(s) {
  if (settingsPage === 'about') return aboutPage(s);
  if (settingsPage === 'controllers') return controllersPage(s);
  const store = s.stores.find((st) => st.id === settingsPage);
  return store ? storePage(store) : generalPage(s);
}

/** The settings for every game: the window, new games, and how launches follow a game. */
function generalPage(s) {
  const launch = draft.launch;
  const timing = (field, title, help, choices, format) => settingRow(title, help,
    select(title, timings(choices, launch[field], format), String(launch[field]), (v) => changeDraft((d) => { d.launch[field] = Number(v); })));
  const ms = (v) => (v < 1000 ? t('general.unit_ms', { value: v }) : t('general.unit_s', { value: v / 1000 }));
  const overlayOff = s.steamOverlay && !s.steamOverlay.enabled && checkRow({
    state: 'warn',
    name: t('general.overlay_off_title'),
    text: t('general.overlay_off_text'),
  });
  return [
    h('h2', { 'data-drag': true }, t('settings.general')),
    overlayOff && h('div.checks', {}, overlayOff),
    h('h3', {}, t('general.window')),
    settingRow(h('span', {}, svgIcon(ICONS.globe, 'row-icon'), t('language.title')), t('language.help'), languagePicker(s)),
    settingRow(t('general.text_size'), t('general.text_size_help', { size: s.autoTextSize }),
      select(t('general.text_size'), [['auto', t('general.text_size_auto', { size: s.autoTextSize })], ...TEXT_SIZES.map((n) => [String(n), t('general.percent', { value: n })])],
        draft.textSize === null ? 'auto' : String(draft.textSize),
        (v) => changeDraft((d) => { d.textSize = v === 'auto' ? null : Number(v); }))),
    h('h3', {}, t('general.new_games')),
    settingRow(t('general.notices_title'), t('general.notices_help'),
      toggle(draft.newGameNotices, (on) => changeDraft((d) => { d.newGameNotices = on; }))),
    h('h3', {}, t('general.launches')),
    h('p', {}, t('general.launches_help')),
    timing('checkMs', t('general.check_title'), t('general.check_help'), [250, 500, 1000, 2000, 5000], ms),
    timing('startTimeoutS', t('general.start_wait_title'), t('general.start_wait_help'), START_WAITS, seconds),
    timing('endGraceS', t('general.end_title'), t('general.end_help'), END_WAITS, seconds),
    timing('questionTimeoutS', t('general.answer_title'), t('general.answer_help'), [30, 60, 120, 300, 600], seconds),
    timing('handOverS', t('general.hand_over_title'), t('general.hand_over_help'), HAND_OVER_WAITS, seconds),
  ];
}

/** The Controllers page: the driver's check and the table of controllers (`controllerTable`). Every state comes from Rust. */
function controllersPage(s) {
  return [
    h('h2', { 'data-drag': true }, t('settings.controllers')),
    h('p', {}, t('controllers.intro')),
    h('div.checks', {}, checkRow(driverCheck(s.driver))),
    s.controllers.length
      ? controllerTable(s)
      : h('p.fine', {}, t('controllers.none_found')),
  ];
}

/**
 * A store's settings. Each control shows the draft's choice, else the store's saved value
 * (`values`, from plan.rs). A choice turned back to the saved value goes back to the saved choice,
 * so it changes nothing.
 */
function storePage(st) {
  const choices = draft.stores[st.id];
  const value = (field) => choices[field] ?? st.values[field];
  const pick = (field, v) => changeDraft((d) => { d.stores[st.id][field] = v === st.values[field] ? st.settings[field] : v; });
  const rows = [];
  if (st.app) {
    const parts = appParts(st.app, st.id, st.closesItself);
    // The choice lists hold keys (or English while the texts load): t() gives back what it can't find.
    const options = (list) => list.map(([v, text]) => [v, t(text)]);
    rows.push(settingRow(...parts.appRunning, select(t('store.app_running_label'), options(APP_RUNNING), value('appRunning'), (v) => pick('appRunning', v))));
    rows.push(settingRow(...parts.handOver, select(t('store.hand_over_label'), options(HAND_OVER), value('handOver'), (v) => pick('handOver', v))));
    rows.push(settingRow(...parts.closeApp, toggle(value('closeApp'), (on) => pick('closeApp', on))));
  }
  if (st.asksAgain) {
    // The store's own wait, or the general one (`launch.nudgeS`, which no page shows on its own).
    const [title, help] = nudgeParts(st);
    const own = choices.nudgeS ?? null;
    rows.push(settingRow(title, help,
      select(title, [['default', t('store.default_wait', { value: seconds(draft.launch.nudgeS) })], ...timings(NUDGE_WAITS, own ?? draft.launch.nudgeS, seconds)],
        own === null ? 'default' : String(own), (v) => changeDraft((d) => { d.stores[st.id].nudgeS = v === 'default' ? null : Number(v); }))));
  }
  rows.push(h('p.subhead', {}, unseenLine(st, 'store')));
  for (const [field, title, help] of standInParts()) {
    rows.push(settingRow(title, help, toggle(value(field), (on) => pick(field, on))));
  }
  if (st.apps) {
    rows.push(h('p.subhead', {}, t('store.apps_line', { count: st.apps, store: storeName(st.id, st.name) })));
    rows.push(settingRow(t('store.full_screen'), t(FULL_SCREEN_HELP), toggle(value('fullScreen'), (on) => pick('fullScreen', on))));
  }
  const found = st.games ? t('store.games_found', { count: st.games }) : t('store.no_games');
  return [
    h('h2', { 'data-drag': true }, storeLogo(st.id), storeName(st.id, st.name)),
    h('p', {}, [st.id === 'exe' && t('store.exe_intro'), found, t('store.page_hint')].filter(Boolean).join(' ')),
    ...rows,
  ];
}

function aboutPage(s) {
  return [
    h('div.about', {},
      h('img.about-cloud', { src: 'logo-wink.svg', alt: '', draggable: 'false' }),
      h('h2', { 'data-drag': true }, 'steambro'),
      h('p.version', {}, t('about.version', { version: s.version })),
      h('p', {}, t('about.license_text')),
      h('div.links', {},
        h('a', { href: '#', onclick: (e) => { e.preventDefault(); openLink('license'); } }, t('about.license')),
        h('a', { href: '#', onclick: (e) => { e.preventDefault(); openLink('source'); } }, t('about.github')))),
    h('h3', {}, t('about.trouble_title')),
    h('p', {}, t('about.trouble_text')),
    h('div.links', {}, h('button', { onclick: saveReport }, t('about.save_report'))),
    h('h3', {}, t('about.start_fresh')),
    h('p', {}, t('about.start_fresh_text')),
    h('div.links', {}, h('button.danger', { onclick: startFresh, disabled: busy }, t('about.start_fresh'))),
  ];
}

/**
 * Starts steambro again as if just installed (`start_fresh`): every game it added leaves Steam,
 * with its artwork, its virtual controllers leave Steam's blocklist, and its data goes but the
 * logs and reports.
 */
async function startFresh() {
  const ok = await ask({
    title: t('dialog.start_fresh_title'),
    text: [t('dialog.start_fresh_text'), view.steam.running && t('dialog.steam_restarts'), t('dialog.logs_stay')].filter(Boolean).join(' '),
    confirm: t('about.start_fresh'),
    tone: 'danger',
  });
  if (!ok) return;
  toast(view.steam.running ? t('toast.starting_fresh_steam') : t('toast.starting_fresh'));
  try {
    localStorage.clear();
  } catch {
    // Only conveniences live there.
  }
  draft = null;
  try {
    await invoke('start_fresh');
  } catch (error) {
    toast(t('toast.start_fresh_failed', { error }), 'error');
  }
}

/** Writes a report for a bug (src/report.rs); Explorer shows it. */
async function saveReport() {
  toast(t('toast.report_writing'));
  try {
    await invoke('save_report');
    toast(t('toast.report_saved'), 'ok');
  } catch (error) {
    toast(t('toast.report_failed', { error }), 'error');
  }
}

/** Adds a game by its exe, chosen in Windows' file dialog, and opens its page. */
async function addExe() {
  try {
    const key = await invoke('add_exe');
    if (!key) return;
    await load();
    if (game(key)) openGame(key);
  } catch (error) {
    toast(t('toast.add_exe_failed', { error }), 'error');
  }
}

async function forgetExe(g) {
  const ok = await ask({
    title: t('dialog.forget_exe_title', { name: g.name }),
    text: g.status === 'steambro' ? t('dialog.forget_exe_in_steam') : t('dialog.forget_exe_text'),
    confirm: t('dialog.forget'),
  });
  if (!ok) return;
  try {
    await invoke('forget_exe', { key: g.key });
    mode = 'tiles';
    openKey = null;
    await load();
  } catch (error) {
    toast(t('toast.forget_exe_failed', { error }), 'error');
  }
}

function openLink(what) {
  invoke('open_link', { what }).catch((error) => toast(t('toast.open_link_failed', { error }), 'error'));
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

// ---------------------------------------------------------------- controller navigation

// A controller moves through the window as the keyboard's focus: the D-pad or the left stick move
// among what can be pressed, in the direction pressed (`spatialTarget`), and the buttons act on the
// focused element (`padPress`). The poll reads the controllers every frame, like the choice
// dialog's own in choice.js, and rests while that dialog is open.
//
// Outside a game, Steam's desktop layout turns a Steam Controller into keys and a mouse, so it never
// shows up as a controller here: its D-pad sends the arrow keys, A Enter, B Escape, and Y Space. So
// the arrow keys move the same way, from anywhere but a text field's caret, and Enter works a select
// or a switch like A. While Rust says a controller is connected (`controllers` in the settings
// view), the keys show the controller's marks too, only for what those keys do (`body.pad-keys`).
// An Xbox or PlayStation controller reaches the window both ways at once; choice.js drops the key.
//
// The hints in the corner say what each button does where the focus is (`renderPadHints`); the
// buttons that one press reaches carry its mark while a controller moves (`buttonMark`).
//
// Every render replaces its elements, which drops the focus. So the window remembers what has the
// focus by a key (`focusKey`) and gives it back after a render (`restorePadFocus`), as long as the
// last input wasn't the mouse.

// The controller's buttons in the Gamepad API's standard mapping, by what they are here.
const PAD_BUTTONS = [[0, 'a'], [1, 'b'], [2, 'x'], [3, 'y'], [4, 'lb'], [5, 'rb'], [8, 'back'], [9, 'start'], [12, 'up'], [13, 'down'], [14, 'left'], [15, 'right']];
const DIRECTIONS = ['up', 'down', 'left', 'right'];
// A direction held repeats: the first time after this long, then at this pace, in milliseconds.
const REPEAT_AFTER = 400;
const REPEAT_EVERY = 110;
// The stick counts as pushed past this much of its way.
const DEAD_ZONE = 0.6;
const ARROWS = { ArrowUp: 'up', ArrowDown: 'down', ArrowLeft: 'left', ArrowRight: 'right' };

// What a controller can reach, by what is open (`padScope`): the right-click menu, the settings
// window, or the main window. Hidden, disabled, and empty elements are left out (`reachable`).
const PAD_TARGETS = {
  menu: '#menu .menu-item:not(:disabled)',
  settings: '#settings .page-link, #settings .nav-support button, #settings .settings-page select, #settings .settings-page input.switch, #settings .settings-page button, #settings .settings-page a[href], #settings .settings-bar button',
  // The window buttons are left out: a controller has B to go back, and the tray to close.
  main: ['#open-settings', '#controller-alert button', '#steam-changes button', '#all-games', '#refresh', '#add-exe',
    '#sections .section-link', '#sections .fold', '#sections .game-row', '#back', '#search', '#selection button',
    '#tags .tag', '#library .card:not([hidden])', '.summary-actions button', '.tabs .tab', '.tab-page select',
    '.tab-page input', '.tab-page button', '.tab-page a[href]', '#notices button'].join(', '),
};
// Up and down stay in their column: the sidebar, the content, the settings window's pages, its open
// page, or its bar. Otherwise up from the first card would land in the sidebar, and down from the
// middle of a long settings page in Cancel, which lies nearer than the rows scrolled out below it.
// Left and right cross them. Up or down with nothing ahead in the column goes on in the larger one
// around it: from the settings page's last row down to its bar, and back up.
const COLUMNS = '.sidebar, .content, .settings-nav, .settings-page, .settings-bar';
const OUTER_COLUMNS = '.sidebar, .content, .settings-nav, .settings-main';

// The last input was the mouse: then no render and no page moves the focus by itself.
let mouseLast = true;
// The element focused last and its key, to find it again after a render replaced it.
let padLast = null;
let padKey = null;
// What had the focus before the controller opened a menu, to get it back when the menu closes.
let padReturn = null;
// The hints in the corner; settings renders move them into its bar and back.
const padStrip = $('#pad-hints');

/**
 * A controller moves the focus now, through the Gamepad API (`pad`) or as the keys of Steam's
 * desktop layout (`keys`): the window shows the controller's marks and hints. Keys show them only
 * while a controller is connected; without one, they come from a keyboard.
 */
function padMode(via) {
  mouseLast = false;
  const marks = via === 'pad' || controllerConnected();
  document.body.classList.toggle('pad-nav', marks);
  document.body.classList.toggle('pad-keys', marks && via === 'keys');
  // The choice dialogs show their marks from now on too.
  if (marks) padSeen = true;
  renderPadHints();
}

/** Rust's check found a controller connected (the Controllers page of the settings). */
function controllerConnected() {
  return Boolean(settingsView && Array.isArray(settingsView.controllers) && settingsView.controllers.some((c) => c.connected));
}

/**
 * A controller button's mark on the button it presses, shown while a controller moves. `keys`:
 * the desktop layout's keys press it too (A Enter, B Escape, Y Space); else the mark hides then.
 */
function buttonMark(letter, keys = false) {
  const wide = !/^[ABXY]$/.test(letter);
  return h(`span.pad.pad-mark${wide ? '.pad-wide' : `.pad-${letter.toLowerCase()}`}${keys ? '' : '.pad-only'}`, { 'aria-hidden': 'true' }, letter);
}

/** The game page's main action, which Y presses: Add to Steam, Remove from Steam, and the like. */
function primaryAction() {
  return mode === 'game' && padScope() === 'main' ? [...document.querySelectorAll('#detail .summary-actions button')].find(reachable) || null : null;
}

/**
 * The hints in the corner: what each button does where the focus is, in the settings window's bar
 * while it is open. With the desktop layout's keys, only A, B, and Y on a card, which those keys do.
 */
function renderPadHints() {
  const scope = padScope();
  const host = scope === 'settings' ? $('#settings .settings-bar') : $('.content');
  if (host && padStrip.parentElement !== host) host.prepend(padStrip);
  if (!document.body.classList.contains('pad-nav')) return;
  const keys = document.body.classList.contains('pad-keys');
  const focused = padFocused(padTargets(scope));
  const hints = [];
  const add = (marks, text, fromKeys = false) => { if (!keys || fromKeys) hints.push([marks, text]); };
  if (scope === 'menu') {
    add(['A'], t('pad.choose'), true);
    add(['B'], t('pad.close'), true);
  } else if (scope === 'settings') {
    // Cancel and Save carry their B and Y marks; the bar has little room.
    add(['A'], t('pad.choose'), true);
    add(['LB', 'RB'], t('pad.pages'));
  } else if (mode === 'game') {
    add(['A'], t('pad.choose'), true);
    add(['B'], t('pad.back'), true);
    const main = primaryAction();
    if (main) add(['Y'], main.dataset.hint || main.textContent.trim());
    if ($('#detail .tabs .tab')) add(['LB', 'RB'], t('pad.tabs'));
    add(['☰'], t('pad.settings'));
  } else {
    const onGame = Boolean(focused && focused.matches('.card, .game-row'));
    add(['A'], t(onGame ? 'pad.open' : 'pad.choose'), true);
    if (selected.size || $('#search').value) add(['B'], t('pad.back'), true);
    if (onGame) add(['X'], t('pad.menu'));
    if (focused && focused.matches('.card')) add(['Y'], t('pad.select'), true);
    if (view && storeTags().length > 1) add(['LB', 'RB'], t('pad.stores'));
    add(['⧉'], t('pad.search'));
    add(['☰'], t('pad.settings'));
  }
  const mark = (m) => h(`span.pad${/^[ABXY]$/.test(m) ? `.pad-${m.toLowerCase()}` : '.pad-wide'}`, {}, m);
  padStrip.replaceChildren(...hints.map(([marks, text]) => h('span.hint', {}, ...marks.map(mark), h('span', {}, text))));
}

function padScope() {
  if (!$('#menu').hidden) return 'menu';
  if ($('#settings').open) return 'settings';
  return 'main';
}

/** Shown and enabled, so a press can reach it. The elements of a hidden view have no size. */
function reachable(el) {
  if (!el || el.disabled || el.closest('[hidden]')) return false;
  const box = el.getBoundingClientRect();
  return box.width > 0 && box.height > 0;
}

function padTargets(scope = padScope()) {
  return [...document.querySelectorAll(PAD_TARGETS[scope])].filter(reachable);
}

/** The target that has the focus, or holds it, like a card whose checkbox has it. */
function padFocused(targets) {
  const active = document.activeElement;
  return targets.find((t) => t === active || t.contains(active)) || null;
}

/** Gives `el` the focus and scrolls just enough to show it. */
function padFocus(el) {
  if (!el) return null;
  el.focus({ preventScroll: true });
  el.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  return el;
}

/**
 * Where the focus goes once a page opens: the first element that one of `selectors` matches, the
 * first selector that matches anything. Not when the mouse opened the page.
 */
function padLand(...selectors) {
  if (mouseLast) return null;
  for (const selector of selectors) {
    const el = [...document.querySelectorAll(selector)].find(reachable);
    if (el) return padFocus(el);
  }
  return null;
}

/**
 * How to find an element again once a render replaced it: by its ID, or by what names it among
 * its kind: a game's key on cards and rows, a controller's ID on a check row's button, the page on
 * a settings page's link, the tab on a game page's tab, the section on the sidebar's folds and
 * section links, the store on a tag chip. Else by its label.
 */
function focusKey(el) {
  if (el.id) return { selector: `#${CSS.escape(el.id)}` };
  const name = ['key', 'page', 'tab', 'section', 'tag'].find((n) => el.dataset[n] !== undefined);
  if (name) {
    const kind = el.classList.length ? `.${el.classList[0]}` : el.tagName.toLowerCase();
    return { selector: `${kind}[data-${name}="${CSS.escape(el.dataset[name])}"]` };
  }
  return { label: focusLabel(el) };
}

/** What names a control without a key: its label, or for a switch, its setting row's text. */
function focusLabel(el) {
  return el.getAttribute('aria-label') || (el.closest('label.setting') || el).textContent.trim();
}

/**
 * After a render replaced the focused element: the element with the same key gets the focus, if
 * the last input wasn't the mouse. A focused element that is still there keeps it.
 */
function restorePadFocus() {
  if (mouseLast || !padLast || padLast.isConnected) return;
  const targets = padTargets();
  const el = padKey.selector ? targets.find((t) => t.matches(padKey.selector)) : targets.find((t) => focusLabel(t) === padKey.label);
  if (el) padFocus(el);
}

/**
 * The target a direction leads to from `from`: the nearest one ahead in that direction, or none.
 * Up and down go by rows, in the element's column: the nearest row counts far more than how far
 * to the side it lies, so that a switch on the left of a settings row is reached from a select on
 * the right of the row above. Left and right go by the element's own row: a target that shares its
 * height comes before any nearer one above or below, so that a sidebar row leads to the card
 * beside it, not to the way back in the window's corner.
 */
function spatialTarget(from, direction, targets) {
  const vertical = direction === 'up' || direction === 'down';
  if (!vertical) return nearestAhead(from, direction, targets);
  const inner = from.closest(COLUMNS);
  const outer = from.closest(OUTER_COLUMNS);
  return nearestAhead(from, direction, targets.filter((el) => el.closest(COLUMNS) === inner))
    || (outer && outer !== inner ? nearestAhead(from, direction, targets.filter((el) => el.closest(OUTER_COLUMNS) === outer)) : null);
}

/** The nearest of `targets` ahead of `from` in `direction`, scored as `spatialTarget` says. */
function nearestAhead(from, direction, targets) {
  const a = from.getBoundingClientRect();
  const vertical = direction === 'up' || direction === 'down';
  let best = null;
  let bestScore = Infinity;
  for (const el of targets) {
    if (el === from) continue;
    const b = el.getBoundingClientRect();
    // The gap between the facing edges; behind, or in the same row, doesn't count.
    const ahead = { up: a.top - b.bottom, down: b.top - a.bottom, left: a.left - b.right, right: b.left - a.right }[direction];
    if (ahead < -1) continue;
    // The gap to the side, 0 while the two share the cross axis, and how far apart their middles are.
    const beside = Math.max(0, vertical ? Math.max(a.left, b.left) - Math.min(a.right, b.right) : Math.max(a.top, b.top) - Math.min(a.bottom, b.bottom));
    const middles = vertical ? Math.abs(a.left + a.right - b.left - b.right) : Math.abs(a.top + a.bottom - b.top - b.bottom);
    const score = vertical
      ? ahead + beside * 0.05 + middles * 0.01
      : (beside > 0 ? 1e4 + beside : 0) + ahead + middles * 0.01;
    if (score < bestScore) {
      best = el;
      bestScore = score;
    }
  }
  return best;
}

/** Where a direction starts when nothing has the focus: the open page's first thing. */
function padStart(scope, targets) {
  const first = (selector) => targets.find((t) => t.matches(selector));
  if (scope === 'settings') return first('.page-link[aria-current="page"]') || targets[0];
  if (mode === 'game') return first('.tabs .tab.on') || first('#detail *') || targets[0];
  return first('#library .card') || targets[0];
}

/** Moves the focus in `direction`; on a select, left and right change its value instead. */
function padMove(direction, scope, targets) {
  const focused = padFocused(targets);
  if (!focused) {
    padFocus(padStart(scope, targets));
  } else if (focused instanceof HTMLSelectElement && (direction === 'left' || direction === 'right')) {
    stepSelect(focused, direction === 'right' ? 1 : -1, false);
  } else {
    padFocus(spatialTarget(focused, direction, targets));
  }
}

/** Moves a select to another option and tells its listener: a controller can't open its list. */
function stepSelect(sel, step, wrap) {
  const n = sel.options.length;
  let i = sel.selectedIndex + step;
  if (wrap) i = (i + n) % n;
  if (i < 0 || i >= n || i === sel.selectedIndex) return;
  sel.selectedIndex = i;
  sel.dispatchEvent(new Event('change'));
}

/** A controller button, pressed or repeating. */
function padPress(key) {
  padMode('pad');
  padAct(key);
  // What the press did, like a selection or a page that opened, changes the hints.
  requestAnimationFrame(renderPadHints);
}

/** What a button does where the focus is: in the menu, the settings window, or the main window. */
function padAct(key) {
  const scope = padScope();
  if (scope === 'menu') return menuPress(key);
  const targets = padTargets(scope);
  if (DIRECTIONS.includes(key)) return padMove(key, scope, targets);
  const focused = padFocused(targets);
  switch (key) {
    case 'a': return padActivate(focused);
    case 'b': return padBack(scope);
    case 'x': return padMenu(focused);
    case 'y': return padY(scope, focused);
    case 'lb': return padStep(scope, -1);
    case 'rb': return padStep(scope, 1);
    case 'start': return scope === 'settings' ? closeSettings() : openSettings();
    case 'back': return padSearch();
    default: return undefined;
  }
}

/** A: presses the focused element; a select goes to its next option. */
function padActivate(focused) {
  if (!focused) return;
  if (focused instanceof HTMLSelectElement) stepSelect(focused, 1, true);
  else focused.click();
}

/** B: one step back, from the settings window, a game's page, a selection, or the search. */
function padBack(scope) {
  if (scope === 'settings') {
    closeSettings();
    return;
  }
  const search = $('#search');
  if (mode === 'game') closeGame();
  else if (selected.size) clearSelection();
  else if (search.value) {
    search.value = '';
    search.dispatchEvent(new Event('input'));
  }
}

/** X: the game's menu, as a right click on the focused card or row, from its middle. */
function padMenu(focused) {
  const g = focused && focused.matches('.card, .game-row') && game(focused.dataset.key);
  if (!g) return;
  padReturn = focused;
  const box = focused.getBoundingClientRect();
  openCardMenu(g, box.left + box.width / 2, box.top + box.height / 2);
}

/** Y: Save in the settings, a game page's main action, or on the tiles, selects the focused card. */
function padY(scope, focused) {
  if (scope === 'settings') {
    const save = $('#settings .save');
    if (save && !save.disabled) save.click();
    return;
  }
  if (mode === 'game') {
    primaryAction()?.click();
    return;
  }
  const g = mode === 'tiles' && focused && focused.matches('.card') && game(focused.dataset.key);
  if (g) toggleSelected(g);
}

/** LB and RB: the page before or after in the settings, the tab on a game's page, the store tag on the tiles. */
function padStep(scope, step) {
  // The element `step` away from the current one in a list, or null at its end.
  const next = (selector, current) => {
    const all = [...document.querySelectorAll(selector)];
    const at = all.findIndex(current);
    const to = Math.min(Math.max(at + step, 0), all.length - 1);
    return to === at ? null : all[to];
  };
  if (scope === 'settings') {
    next('#settings .page-link', (l) => l.getAttribute('aria-current') === 'page')?.click();
    padFocus($('#settings .page-link[aria-current="page"]'));
  } else if (mode === 'game') {
    next('.tabs .tab', (t) => t.classList.contains('on'))?.click();
    padFocus($('.tabs .tab.on'));
  } else if (view) {
    // One store at a time, and All before the first.
    const stores = storeTags();
    const at = tags.size === 1 ? stores.findIndex((t) => tags.has(t.id)) : -1;
    const to = Math.min(Math.max(at + step, -1), stores.length - 1);
    tags.clear();
    if (to >= 0) tags.add(stores[to].id);
    applyFilter();
    // The focused card may be filtered out now.
    if (!reachable(document.activeElement)) padFocus($('#library .card:not([hidden])') || $('#tags .tag[aria-pressed="true"]'));
  }
}

/** View: the search, from anywhere; pressed there, it clears the search. */
function padSearch() {
  const search = $('#search');
  if (document.activeElement === search) {
    if (search.value) {
      search.value = '';
      search.dispatchEvent(new Event('input'));
    }
    return;
  }
  if (mode !== 'tiles') openTiles(openSection);
  padFocus(search);
}

/**
 * The right-click menu has its own keys (openMenu); the controller sends them, without bubbling,
 * so the window's own Escape doesn't see them. Once the menu is closed, the focus goes back to
 * where it was, unless what the menu did has placed it already.
 */
function menuPress(key) {
  const menu = $('#menu');
  const send = (k) => menu.dispatchEvent(new KeyboardEvent('keydown', { key: k, cancelable: true }));
  if (key === 'up') send('ArrowUp');
  else if (key === 'down') send('ArrowDown');
  else if (key === 'b') send('Escape');
  else if (key === 'a') document.activeElement?.closest('.menu-item')?.click();
  if (menu.hidden) {
    if (reachable(padReturn) && !padTargets().includes(document.activeElement)) padFocus(padReturn);
    padReturn = null;
  }
}

/** The buttons and the stick a controller holds now, by name. */
function padKeys(pad) {
  const keys = new Set();
  for (const [i, key] of PAD_BUTTONS) if (pad.buttons[i] && pad.buttons[i].pressed) keys.add(key);
  const [x = 0, y = 0] = pad.axes;
  if (y < -DEAD_ZONE) keys.add('up');
  if (y > DEAD_ZONE) keys.add('down');
  if (x < -DEAD_ZONE) keys.add('left');
  if (x > DEAD_ZONE) keys.add('right');
  return keys;
}

/**
 * Reads the controllers every frame. A button held when the poll starts, or when the choice
 * dialog closes, counts only after it is let go; a direction held repeats.
 */
function padPoll() {
  // By controller and key: when it was pressed, and when it last counted.
  const held = new Map();
  let primed = false;
  const frame = () => {
    // Scheduled first, so that an error in a press doesn't end the poll.
    requestAnimationFrame(frame);
    if ($('#dialog').open) {
      // The choice dialog reads the controllers itself (choice.js).
      held.clear();
      primed = false;
      return;
    }
    const now = performance.now();
    for (const pad of standardPads()) {
      if (!pad) continue;
      const keys = padKeys(pad);
      for (const key of keys) {
        const id = `${pad.index}:${key}`;
        const press = held.get(id);
        if (!press) {
          held.set(id, { since: now, fired: now });
          // The desktop layout's key for the same press may have come first (choice.js).
          if (primed && !keyEchoed(key)) padPress(key);
        } else if (DIRECTIONS.includes(key) && now - press.since >= REPEAT_AFTER && now - press.fired >= REPEAT_EVERY) {
          press.fired = now;
          padPress(key);
        }
      }
      for (const id of [...held.keys()]) {
        if (id.startsWith(`${pad.index}:`) && !keys.has(id.slice(id.indexOf(':') + 1))) held.delete(id);
      }
    }
    primed = true;
  };
  requestAnimationFrame(frame);
}

/** Starts the controller poll, and the listeners that keep the focus in step with it and with the keyboard. */
function padNavigation() {
  document.addEventListener('focusin', (e) => {
    padLast = e.target;
    padKey = focusKey(e.target);
    renderPadHints();
  });
  $('#settings').addEventListener('close', renderPadHints);
  // A mouse that moves ends the controller's turn. The browser also sends a mouse move when what
  // lies under a still mouse changes, like after a scroll, at the same position; that one doesn't count.
  let mouseAt = null;
  const mouse = (e) => {
    const at = `${e.screenX},${e.screenY}`;
    if (e.type === 'mousemove' && at === mouseAt) return;
    mouseAt = at;
    mouseLast = true;
    document.body.classList.remove('pad-nav');
  };
  document.addEventListener('mousemove', mouse);
  document.addEventListener('mousedown', mouse);
  // The arrow keys move like the D-pad, also when nothing has the focus yet: then the first press
  // starts at the open page's first thing. A text field keeps left and right for its caret; a
  // select changes with left and right, as with the controller, and up and down leave it. Enter
  // works a select or a switch like A. The choice dialog and the menu have their own keys.
  document.addEventListener('keydown', (e) => {
    mouseLast = false;
    if (e.altKey || e.ctrlKey || e.metaKey || e.shiftKey || $('#dialog').open || !$('#menu').hidden) return;
    const target = e.target instanceof Element ? e.target : document.body;
    const key = ARROWS[e.key];
    if (key) {
      if ((key === 'left' || key === 'right') && target.closest('input:not([type="checkbox"]), textarea')) return;
      e.preventDefault();
      padMode('keys');
      const scope = padScope();
      padMove(key, scope, padTargets(scope));
    } else if (e.key === 'Enter' && target.matches('select, input[type="checkbox"]')) {
      e.preventDefault();
      if (!e.repeat) padActivate(target);
    }
  });
  // A key may change what the hints say, like Space selecting a card.
  document.addEventListener('keyup', () => {
    if (document.body.classList.contains('pad-nav')) renderPadHints();
  });
  // The mouse's back button goes one step back, like B. Where it acts is decided when it goes down:
  // the right-click menu closes on any press outside it, before the button comes up, and then the
  // press only closed the menu.
  let backScope = null;
  window.addEventListener('pointerdown', (e) => {
    if (e.button === 3) backScope = padScope();
  }, true);
  document.addEventListener('mouseup', (e) => {
    if (e.button !== 3) return;
    const scope = backScope;
    backScope = null;
    if ($('#dialog').open || !scope) return;
    e.preventDefault();
    if (scope === 'menu') {
      if (!$('#menu').hidden) menuPress('b');
    } else {
      padBack(scope);
    }
  });
  padPoll();
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
$('#open-settings').addEventListener('click', () => openSettings());
$('#back').addEventListener('click', closeGame);
// Escape is Cancel.
$('#settings').addEventListener('cancel', (e) => {
  e.preventDefault();
  closeSettings();
});
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
// The browser's own menu (back, reload, inspect) has no place here; text fields keep theirs.
document.addEventListener('contextmenu', (e) => {
  if (!e.target.closest('input, textarea')) e.preventDefault();
});
document.addEventListener('keydown', (e) => {
  const typing = e.target.closest('input, textarea, select');
  const modal = $('#dialog').open || $('#settings').open || !$('#menu').hidden;
  if (modal) return;
  if (e.key === 'Escape') {
    // As B: the game's page, the selection, or the search. Steam's desktop layout sends B as Escape.
    padBack('main');
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
// The stores had newer artwork (refresh_art in src/app/mod.rs): the images are new too.
listen('art-changed', () => {
  artVersion = Date.now();
  if (!busy) load();
});
listen('progress-game', ({ payload }) => changeStep(payload.key, payload.step));
listen('progress-steam', ({ payload }) => changeStage(payload));
// Look for new games whenever the window comes back from the tray.
listen('window-shown', () => {
  if (!busy) load(true);
  loadSettings();
});
pokeLogo();
windowControls();
padNavigation();
// The texts come first: everything rendered reads them through t().
(async () => {
  try {
    await loadStrings();
  } catch (error) {
    toast(`steambro couldn't read its texts: ${error}`, 'error');
  }
  translatePage();
  skeleton();
  load();
  loadSettings();
})();
