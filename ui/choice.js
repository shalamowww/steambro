'use strict';

// steambro's choice dialog, shared by the main window (app.js) and the question window of a game
// launch (prompt.js). Cancel is always on the left, the cloud sits between the buttons: it winks while
// nothing is picked, glances at Cancel (its eyes look left), and squeezes its eyes for the
// confirming button. Mouse, keyboard, and controller work: A picks the highlighted button (or
// confirms), B cancels, left and right move between the buttons, up and down move between the
// choices, and Y toggles the checkbox.

/** Creates an element: `h('button.primary', { onclick }, 'Text', child)`. */
function h(tag, attrs = {}, ...children) {
  const [name, ...classes] = tag.split('.');
  const node = document.createElement(name);
  if (classes.length) node.className = classes.join(' ');
  for (const [key, value] of Object.entries(attrs)) {
    if (value === undefined || value === null || value === false) continue;
    if (key.startsWith('on')) node.addEventListener(key.slice(2), value);
    else if (key === 'style') Object.assign(node.style, value);
    else node.setAttribute(key, value === true ? '' : value);
  }
  node.append(...children.flat().filter((c) => c !== null && c !== undefined && c !== false));
  return node;
}

// A controller has been used in this window, so the dialogs show its buttons.
let padSeen = false;
window.addEventListener('gamepadconnected', () => { padSeen = true; });

/** A controller button's mark: `A`, `B`, or `Y`, colored like an Xbox controller's. */
function padMark(letter) {
  return h(`span.pad.pad-${letter.toLowerCase()}`, { 'aria-hidden': 'true' }, letter);
}

// Outside a game, Steam's desktop layout turns a controller's buttons into keys, while the Gamepad
// API reads an Xbox or PlayStation controller too: one press arrives twice. Steam's layouts send
// the arrows for the D-pad and the left stick, Enter for A, Escape for B, Space for Y, Backspace
// for X, Delete for View, and Tab for Menu; older ones Space for B, Page Up and Page Down for X and
// Y, Escape for View. The Gamepad API sees a press up to a frame or two before or after its key.
//
// - A key whose button went down just before is the echo, and it is dropped (`padEcho`).
// - A key that does what its button does (the arrows, Enter, Escape) may come first; then the
//   button is the echo, and the polls skip it (`keyEchoed`).
// - Space does something else than Y or B: it presses the focused button. While a controller is
//   connected, Space outside a text field waits `ECHO_MS` for one of those buttons, and acts only
//   if none came (`holdSpace`).
// - The other keys do nothing here, so their buttons always act.
// Only controllers with the standard mapping count (`standardPads`): a wheel or pedals can rest on
// a button or an axis.
const ECHOES = {
  ArrowUp: { buttons: [12], axis: [1, -1] },
  ArrowDown: { buttons: [13], axis: [1, 1] },
  ArrowLeft: { buttons: [14], axis: [0, -1] },
  ArrowRight: { buttons: [15], axis: [0, 1] },
  Enter: { buttons: [0] },
  Escape: { buttons: [1, 8] },
  ' ': { buttons: [1, 3] },
  Backspace: { buttons: [2] },
  Delete: { buttons: [8] },
  PageUp: { buttons: [2] },
  PageDown: { buttons: [3] },
  Tab: { buttons: [9] },
};
// The buttons, by the names the polls give them, whose key does the same and may come first.
const ECHOED_BY = { up: ['ArrowUp'], down: ['ArrowDown'], left: ['ArrowLeft'], right: ['ArrowRight'], a: ['Enter'], b: ['Escape'] };
const ECHO_MS = 150;
// A stick direction counts from this far: Steam's layouts send the arrows at a small tilt.
const ECHO_TILT = 0.3;
// When those keys last arrived for real.
const keyAt = new Map();
// The keys dropped, so that their key-ups and repeats go too: a button acts on Space's key-up.
const dropped = new Set();
// When each controller button and stick direction last went down: `<pad>:<button>` or `<pad>:a<axis><sign>`.
const downAt = new Map();
const padsDown = new Set();
// A Space sent again after its wait, which the listener lets through.
let replaying = false;

/** The connected controllers with the standard mapping. */
function standardPads() {
  return [...(navigator.getGamepads ? navigator.getGamepads() : [])].filter((p) => p && p.mapping === 'standard');
}

/** The IDs of what a controller holds now, buttons and stick directions. */
function padHeld() {
  const ids = new Set();
  for (const pad of standardPads()) {
    pad.buttons.forEach((b, i) => { if (b.pressed) ids.add(`${pad.index}:${i}`); });
    pad.axes.slice(0, 2).forEach((v, i) => { if (Math.abs(v) > ECHO_TILT) ids.add(`${pad.index}:a${i}${v > 0 ? '+' : '-'}`); });
  }
  return ids;
}

/** Notes when each button and direction went down, every frame and at each key. */
function trackPads() {
  const now = performance.now();
  const ids = padHeld();
  for (const id of ids) if (!padsDown.has(id)) downAt.set(id, now);
  padsDown.clear();
  for (const id of ids) padsDown.add(id);
}
(function frame() {
  trackPads();
  requestAnimationFrame(frame);
}());

/** A button or direction that sends `key` went down at `since` or later. */
function padEcho(key, since) {
  const echo = ECHOES[key];
  if (!echo) return false;
  trackPads();
  for (const pad of standardPads()) {
    const ids = echo.buttons.map((i) => `${pad.index}:${i}`);
    if (echo.axis) ids.push(`${pad.index}:a${echo.axis[0]}${echo.axis[1] > 0 ? '+' : '-'}`);
    if (ids.some((id) => (downAt.get(id) ?? -Infinity) >= since)) return true;
  }
  return false;
}

/** The key that a controller button sends arrived just now, so the poll's press is the same one. */
function keyEchoed(button) {
  const now = performance.now();
  return (ECHOED_BY[button] || []).some((key) => now - (keyAt.get(key) ?? -Infinity) < ECHO_MS);
}

function dropKey(e) {
  dropped.add(e.code);
  e.preventDefault();
  e.stopImmediatePropagation();
}

/** Space, held back: after `ECHO_MS`, it acts on what had the focus, unless Y or B went down. */
function holdSpace(target, at) {
  setTimeout(() => {
    if (padEcho(' ', at - ECHO_MS) || !target.isConnected) return;
    if (target instanceof HTMLButtonElement || (target instanceof HTMLInputElement && (target.type === 'checkbox' || target.type === 'radio'))) {
      target.click();
      return;
    }
    replaying = true;
    try {
      target.dispatchEvent(new KeyboardEvent('keydown', { key: ' ', code: 'Space', bubbles: true, cancelable: true }));
    } finally {
      replaying = false;
    }
  }, ECHO_MS);
}

window.addEventListener('keydown', (e) => {
  if (replaying) return;
  const now = performance.now();
  if ((e.repeat && dropped.has(e.code)) || padEcho(e.key, now - 3 * ECHO_MS)) {
    dropKey(e);
  } else if (e.key === ' ' && !e.repeat && standardPads().length && e.target instanceof Element
    && !e.target.closest('input:not([type="checkbox"]):not([type="radio"]):not([type="range"]), textarea, select')) {
    dropKey(e);
    holdSpace(e.target, now);
  } else if (ECHOES[e.key]) {
    keyAt.set(e.key, now);
  }
}, true);
window.addEventListener('keyup', (e) => {
  if (dropped.delete(e.code)) {
    e.preventDefault();
    e.stopImmediatePropagation();
  }
}, true);

/**
 * Asks a question in `dialog` (a `<dialog>`). Resolves to false for Cancel, or for the confirm:
 * to true; with `choices`, to the value of the chosen one. With `check`, it always resolves to
 * `{ confirmed, value, checked }`, because "every time" can go with either button.
 *
 * - `account` names the Steam account the action is for.
 * - `choices`: radio options `{ value, label, detail }`; the first is chosen.
 * - `check`: a checkbox `{ label, checked, confirm }`, like "Do this every time"; with `confirm`,
 *   the confirming button says that while the box is ticked.
 * - `cancel`, `confirm`: the buttons' text; Cancel's comes from the texts (t) when left out.
 * - `tone`: the confirm's color: `danger` (red) when it deletes something, `mixed` (purple) when it
 *   adds some things and deletes others.
 * - `pads`: a controller is connected, so the buttons' marks show from the start.
 *
 * Enter with no button focused confirms, and Escape cancels: a controller that Steam turns into
 * keys outside a game, like the Steam Controller, sends those for A and B.
 */
function choose(dialog, { title, text, list, account, choices, check, cancel = t('dialog.cancel'), confirm, tone, pads }) {
  if (pads) padSeen = true;
  return new Promise((resolve) => {
    let done = false;
    let frame = 0;
    const cloud = h('img.choice-cloud', { src: 'logo-wink.svg', alt: '', width: 64, height: 64, draggable: 'false' });
    const cancelButton = h('button.choice-cancel', { type: 'button' }, padMark('B'), h('span', {}, cancel));
    const confirmLabel = h('span', {}, confirm);
    const confirmButton = h(`button.primary.choice-confirm${tone ? `.${tone}` : ''}`, { type: 'button' }, confirmLabel, padMark('A'));
    const checkbox = check && h('input', { type: 'checkbox', checked: check.checked });
    const relabel = () => { if (check && check.confirm) confirmLabel.textContent = checkbox.checked ? check.confirm : confirm; };
    if (checkbox) checkbox.addEventListener('change', relabel);
    relabel();
    const answer = (confirmed) => {
      const value = !confirmed ? false : choices ? dialog.querySelector('input[name="choice"]:checked').value : true;
      return check ? { confirmed, value, checked: checkbox.checked } : value;
    };
    const finish = (confirmed) => {
      if (done) return;
      done = true;
      cancelAnimationFrame(frame);
      // The dialog element is shared with other content, like the crop dialog.
      dialog.onkeydown = dialog.oncancel = dialog.onclose = null;
      const result = answer(confirmed);
      if (dialog.open) dialog.close();
      resolve(result);
    };

    // The cloud follows the button under the mouse, else the focused one.
    let hovered = null;
    const face = () => {
      const on = hovered || (document.activeElement === cancelButton || document.activeElement === confirmButton ? document.activeElement : null);
      const mood = on === cancelButton ? 'glance' : on === confirmButton ? 'squeeze' : 'wink';
      const src = `logo-${mood}.svg`;
      if (!cloud.src.endsWith(src)) cloud.src = src;
    };
    for (const button of [cancelButton, confirmButton]) {
      button.addEventListener('pointerenter', () => { hovered = button; face(); });
      button.addEventListener('pointerleave', () => { hovered = null; face(); });
      button.addEventListener('focus', face);
      button.addEventListener('blur', face);
    }
    cancelButton.addEventListener('click', () => finish(false));
    confirmButton.addEventListener('click', () => finish(true));

    const radios = () => [...dialog.querySelectorAll('input[name="choice"]')];
    const moveChoice = (step) => {
      const all = radios();
      if (!all.length) return;
      const i = all.findIndex((r) => r.checked);
      all[(i + step + all.length) % all.length].checked = true;
    };
    const toggleCheck = () => { if (checkbox) { checkbox.checked = !checkbox.checked; relabel(); } };

    dialog.replaceChildren(
      h('div.dialog-body.choice-body', {},
        h('h3', {}, title),
        text && h('p', {}, text),
        list && h('ul', {}, ...list.map((item) => h('li', {}, item))),
        account && h('p.account-line', {}, t('dialog.account_label'), ' ', h('strong', {}, account)),
        choices && h('div.choices', { role: 'radiogroup' }, ...choices.map((c, i) =>
          h('label.choice', {},
            h('input', { type: 'radio', name: 'choice', value: c.value, checked: i === 0 }),
            h('span', {}, h('strong', {}, c.label), h('small', {}, c.detail))))),
        check && h('label.choice-check', {}, checkbox, h('span', {}, check.label), padMark('Y'))),
      h('div.dialog-actions.choice-actions', {}, cancelButton, cloud, confirmButton));
    dialog.classList.toggle('pad-hints', padSeen);
    dialog.onkeydown = (e) => {
      // A focused radio or checkbox keeps its own keys.
      const own = e.target instanceof HTMLInputElement && (e.target.type === 'radio' || e.target.type === 'checkbox');
      if (e.key === 'ArrowLeft') { e.preventDefault(); cancelButton.focus(); }
      else if (e.key === 'ArrowRight') { e.preventDefault(); confirmButton.focus(); }
      else if ((e.key === 'ArrowUp' || e.key === 'ArrowDown') && !own && radios().length) {
        e.preventDefault();
        moveChoice(e.key === 'ArrowUp' ? -1 : 1);
      } else if (e.key === ' ' && !own && checkbox) {
        // Space is Y on a Steam Controller's desktop layout, the Y mark beside the checkbox.
        e.preventDefault();
        if (!e.repeat) toggleCheck();
      } else if (e.key === 'Enter' && !e.repeat && !(document.activeElement instanceof HTMLButtonElement && dialog.contains(document.activeElement))) {
        e.preventDefault();
        finish(true);
      }
    };
    dialog.oncancel = (e) => { e.preventDefault(); finish(false); };
    dialog.onclose = () => finish(false);
    for (const src of ['logo-glance.svg', 'logo-squeeze.svg']) new Image().src = src;
    dialog.showModal();
    // showModal focuses the first control, and the cloud would start on Cancel. The dialog itself
    // takes the focus instead, so that keys reach its handler.
    dialog.tabIndex = -1;
    dialog.focus();

    // Controller buttons, read every frame while the dialog is open. A button already held when
    // the dialog opens counts only after it is let go.
    const held = new Set();
    let primed = false;
    const pressed = (pad, i) => pad.buttons[i] && pad.buttons[i].pressed;
    const poll = () => {
      if (done) return;
      for (const pad of standardPads()) {
        if (!pad) continue;
        const now = new Set();
        if (pressed(pad, 0)) now.add('a');
        if (pressed(pad, 1)) now.add('b');
        if (pressed(pad, 3)) now.add('y');
        if (pressed(pad, 12) || pad.axes[1] < -0.6) now.add('up');
        if (pressed(pad, 13) || pad.axes[1] > 0.6) now.add('down');
        if (pressed(pad, 14) || pad.axes[0] < -0.6) now.add('left');
        if (pressed(pad, 15) || pad.axes[0] > 0.6) now.add('right');
        for (const key of now) {
          if (!primed || held.has(pad.index + key) || keyEchoed(key)) continue;
          if (!padSeen) { padSeen = true; dialog.classList.add('pad-hints'); }
          if (key === 'a') (document.activeElement === cancelButton ? cancelButton : confirmButton).click();
          else if (key === 'b') finish(false);
          else if (key === 'y') toggleCheck();
          else if (key === 'left') cancelButton.focus();
          else if (key === 'right') confirmButton.focus();
          else if (key === 'up') moveChoice(-1);
          else if (key === 'down') moveChoice(1);
        }
        for (const key of ['a', 'b', 'y', 'up', 'down', 'left', 'right']) {
          if (now.has(key)) held.add(pad.index + key);
          else held.delete(pad.index + key);
        }
      }
      primed = true;
      frame = requestAnimationFrame(poll);
    };
    frame = requestAnimationFrame(poll);
  });
}
