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

/**
 * Asks a question in `dialog` (a `<dialog>`). Resolves to false for Cancel, or for the confirm:
 * to true; with `choices`, to the value of the chosen one. With `check`, it always resolves to
 * `{ confirmed, value, checked }`, because "every time" can go with either button.
 *
 * - `account` names the Steam account the action is for.
 * - `choices`: radio options `{ value, label, detail }`; the first is chosen.
 * - `check`: a checkbox `{ label, checked }`, like "Do this every time".
 * - `cancel`, `confirm`: the buttons' text; `danger` marks a confirm that deletes something.
 */
function choose(dialog, { title, text, list, account, choices, check, cancel = 'Cancel', confirm, danger }) {
  return new Promise((resolve) => {
    let done = false;
    let frame = 0;
    const cloud = h('img.choice-cloud', { src: 'logo-wink.svg', alt: '', width: 64, height: 64, draggable: 'false' });
    const cancelButton = h('button.choice-cancel', { type: 'button' }, padMark('B'), h('span', {}, cancel));
    const confirmButton = h(`button.primary.choice-confirm${danger ? '.danger' : ''}`, { type: 'button' }, h('span', {}, confirm), padMark('A'));
    const checkbox = check && h('input', { type: 'checkbox', checked: check.checked });
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
    const toggleCheck = () => { if (checkbox) checkbox.checked = !checkbox.checked; };

    dialog.replaceChildren(
      h('div.dialog-body.choice-body', {},
        h('h3', {}, title),
        text && h('p', {}, text),
        list && h('ul', {}, ...list.map((item) => h('li', {}, item))),
        account && h('p.account-line', {}, 'Steam account: ', h('strong', {}, account)),
        choices && h('div.choices', { role: 'radiogroup' }, ...choices.map((c, i) =>
          h('label.choice', {},
            h('input', { type: 'radio', name: 'choice', value: c.value, checked: i === 0 }),
            h('span', {}, h('strong', {}, c.label), h('small', {}, c.detail))))),
        check && h('label.choice-check', {}, checkbox, h('span', {}, check.label), padMark('Y'))),
      h('div.dialog-actions.choice-actions', {}, cancelButton, cloud, confirmButton));
    dialog.classList.toggle('pad-hints', padSeen);
    dialog.onkeydown = (e) => {
      if (e.key === 'ArrowLeft') { e.preventDefault(); cancelButton.focus(); }
      else if (e.key === 'ArrowRight') { e.preventDefault(); confirmButton.focus(); }
    };
    dialog.oncancel = (e) => { e.preventDefault(); finish(false); };
    dialog.onclose = () => finish(false);
    for (const src of ['logo-glance.svg', 'logo-squeeze.svg']) new Image().src = src;
    dialog.showModal();
    // Without this, showModal focuses the first control and the cloud would start on Cancel.
    if (document.activeElement && dialog.contains(document.activeElement)) document.activeElement.blur();

    // Controller buttons, read every frame while the dialog is open. A button already held when
    // the dialog opens counts only after it is let go.
    const held = new Set();
    let primed = false;
    const pressed = (pad, i) => pad.buttons[i] && pad.buttons[i].pressed;
    const poll = () => {
      if (done) return;
      for (const pad of navigator.getGamepads ? navigator.getGamepads() : []) {
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
          if (!primed || held.has(pad.index + key)) continue;
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
