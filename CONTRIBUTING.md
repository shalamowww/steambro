# Contributing to steambro

Thanks for helping. steambro is one person's project, so small, focused changes land fastest.

## Report a bug

1. In steambro, open Settings, About, and select "Save a report for a bug".
2. Read it over, then open an [issue](https://github.com/shalamowww/steambro/issues) and attach it. Say what you did, what you expected, and what happened.

The report holds steambro's settings, what it decided for each game, the Steam and store settings that matter, and the end of the logs. Your Windows user name, PC name and Steam account IDs are replaced.

The useful reports: a game that does not start, a controller that does nothing, an overlay that does not open. Name the game, the store and the controller.

## Translate

Every text of the window, the tray menu and the notifications lives in `ui/i18n/<code>.json`. English (`en.json`) is the source. Write the way gamers talk in your language, with the terms Steam's own client uses there. Product names stay as their owners write them.

- To fix a text, change it in that language's file and open a pull request.
- To add a language, copy `en.json` to `<code>.json` and translate every value; keep the `{placeholders}` and the plural forms your language needs. Add the language to `LANGUAGES` and `FILES` in `src/i18n.rs` and to the Windows language check in `system_language`, then run `uv run scripts/check_i18n.py`.

## Change the code

- Open an issue first for anything bigger than a fix, so the design is agreed before the work.
- `cargo build` builds and `cargo test` tests; the README's "Build from source" has the setup. `scripts/dev.ps1 -Restart` rebuilds and restarts a development copy.
- One change per pull request. Match the code around it. A comment says why, not what.
- The window shows what Rust sends and has no rules of its own about stores, settings or controllers. A new rule belongs in Rust.
- Texts go through `t()` and `ui/i18n/en.json`, never as literals in `app.js`.

## License

By contributing, you agree that your contribution is licensed under the [GPL-3.0](LICENSE), like the rest of steambro.
