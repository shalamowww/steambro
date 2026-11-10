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
  const { app, game, storeName, controller } = request;
  try {
    await loadStrings();
  } catch {
    // The English keys' texts come from the fallback inside t(): the window still works.
  }
  const answer = await choose(document.querySelector('#dialog'), {
    pads: controller,
    title: t('prompt.title', { app }),
    text: t('prompt.text', { app, game }),
    check: { label: t('prompt.always', { store: storeName }), checked: false },
    cancel: t('prompt.cancel'),
    confirm: t('prompt.confirm'),
  });
  try {
    await invoke('answer_prompt', { id, restart: answer.confirmed, always: answer.checked });
  } finally {
    win.close();
  }
}

main();
