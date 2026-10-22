<p align="center">
  <img src="assets/icon.png" width="96" alt="JSON Forge icon">
</p>

<h1 align="center">JSON Forge</h1>

<p align="center">A fast desktop viewer and editor for JSON.</p>

JSON Forge opens a JSON file and shows it two ways at once: a collapsible tree on the left for navigation, and a **block view** on the right that turns the document into editable cards. Long strings are shown as plain text instead of escaped literals, nested values get their own cards with the full path in the header, and whatever you edit is written back into the JSON for you.

## Features

- **Block view** – every leaf value is a card you can edit in place. Strings edit as plain text, objects and arrays as JSON that must parse before it is applied. Headers show the JSON path and size, and anything over a couple of thousand characters starts collapsed. A summary card at the top shows the document size and key count.
- **Raw mode** – the whole document in a code editor with line numbers, and parse errors reported by line and column. Switch back to Blocks when it parses again.
- **Tabs** – open as many documents as you like. Opening a file while JSON Forge is already running sends it to the existing window as a new tab.
- **Find and replace** – Atom-style panel at the bottom with regex, case and whole-word toggles, a match counter, Find All to filter blocks down to the ones that match, and Replace / Replace All in either view.
- **Context menu** – right-click in any text field for Cut, Copy, Paste, Select All and Transform Text (UPPER CASE, lower case, Title Case, Sentence case) on the selection or the word under the cursor.
- **Spelling and grammar** – string values are checked as you type: misspellings get a red underline, grammar slips (wrong article, its/it's, subject-verb agreement, repeated words) a blue one. Hover for the explanation, right-click for suggestions, Add to Dictionary or Ignore Word. Checking runs in a separate process, `spellchecker.exe`, which must sit next to `json-forge.exe`; without it the app simply does not check. It skips code spans, URLs and strings that are themselves JSON, and can be turned off in the Edit menu. English only. Your added words live in `%APPDATA%\json-forge\dictionary.txt`.
- **Tree navigation** – click any node in the tree to scroll the block view to it.
- **JSON Lines** – `.jsonl` files and any text that is one JSON value per line load as an array of records and save back the same way.
- **Format and minify**, paste the clipboard as a new document, copy the document, drag and drop, recent files, light and dark themes, and Chrome-style middle-button autoscroll.

## Shortcuts

| Keys | Action |
| --- | --- |
| `Ctrl+O` / `Ctrl+S` / `Ctrl+Shift+S` | Open, save, save as |
| `F5` | Reload the document from disk (after another program changed it) |
| `Ctrl+N` / `Ctrl+W` | New tab, close tab |
| `Ctrl+Tab`, `Ctrl+Shift+Tab`, `Alt+1`…`Alt+9` | Switch tabs |
| `Ctrl+V` (no field focused) | Paste the clipboard as a new document |
| `Ctrl+Shift+C` | Copy the whole document |
| `Ctrl+F` / `Ctrl+Alt+F` | Find, find and replace |
| `F3` / `Shift+F3` | Next / previous match |
| `Ctrl+E` | Use the selection as the search pattern |
| `Ctrl+Alt+/`, `Ctrl+Alt+C`, `Ctrl+Alt+W` | Toggle regex, case sensitivity, whole word |
| `Esc` | Close the find panel |
| Middle click | Autoscroll |

## Building

Requires a Rust toolchain. On Windows:

```
build.bat
```

builds both executables and copies them next to the script. Elsewhere, `cargo build --release` produces `target/release/json-forge` and `target/release/spellchecker`; keep them in the same folder. The app embeds its icons at build time; on Windows that needs the resource compiler that ships with the Windows SDK, which comes with the MSVC toolchain.

## Opening .json files with JSON Forge (Windows)

```
powershell -ExecutionPolicy Bypass -File register.ps1
```

registers JSON Forge for `.json` and `.jsonl` for the current user: the document icon, "Open with" entries and a Default Apps listing. No admin rights needed. Run it again with `-Unregister` to put the previous handler back. The registration points at the exe next to the script, so run it again if you move the folder.

Multiple files can be passed on the command line; each opens in its own tab.

## Built with

[egui](https://github.com/emilk/egui) and [eframe](https://github.com/emilk/egui/tree/master/crates/eframe) for the UI, [egui_json_tree](https://github.com/dmackdev/egui_json_tree) for the tree pane, [serde_json](https://github.com/serde-rs/json) with `preserve_order` so keys stay in file order, [regex](https://github.com/rust-lang/regex) for find and replace, [Harper](https://github.com/Automattic/harper) for spelling and grammar, [rfd](https://github.com/PolyMeilex/rfd) for native dialogs, and [interprocess](https://github.com/kotauskas/interprocess) for the single-window hand-off.

## License

[MIT](LICENSE)
