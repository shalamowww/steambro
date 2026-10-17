'use strict';

// The window that shows a game launch's question (src/prompt.rs): the store's app already runs
// outside Steam, so may steambro restart it inside Steam's launch? The answer goes back to the
// launch, which waits for it; then the window closes.

const { invoke } = window.__TAURI__.core;
const win = window.__TAURI__.window.getCurrentWindow();

async function main() {
  const id = window.PROMPT_ID;
  let request;
  try {
    request = await invoke('prompt_request', { id });
  } catch {
    win.close();
    return;
  }
  const { app, game, storeName } = request;
  const answer = await choose(document.querySelector('#dialog'), {
    title: `Restart ${app}?`,
    text: `${app} is already running outside Steam, so Steam can't see ${game}: no Steam overlay, and Steam Input doesn't reach it. steambro can close ${app} and start it again through Steam. That takes a few seconds more.`,
    check: { label: `Do this every time for ${storeName}`, checked: false },
    cancel: 'Play as it is',
    confirm: 'Restart it',
  });
  try {
    await invoke('answer_prompt', { id, restart: answer.confirmed, always: answer.checked });
  } finally {
    win.close();
  }
}

main();
