# herdr-kaye

A **Herdr (`>= 0.9.5`)** plugin for **KAYE** (local shadow-git VCS): interactive folder explorer with green `+` / red `-` change indicators powered by `kaye what change`, plus a full-width read-only code viewer with inline added/deleted highlights, search, and native Herdr mouse drag-select copy.

## Features

- **Powered by `kaye what change`** — works immediately after `kaye save` checkpoints (shows checkpoint `cp N/N` and saved message in the header, expandable to checkpoint `N` or `all`) and **auto-refreshes** when new checkpoints are saved.
- **Folder & File Explorer** — navigate into folders (`▸*` marks folders with changes and aggregated `+N -M` counts) and see file badges (`M`, `A`, `D`, `R`).
- **Full-Width Read-Only Code View** — opens files full-width with line pointer (`▸`), inline green `+` / red `-` highlights, horizontal panning (`←`/`→` or `h`/`l`), `[1/N]` change counter (`Shift+↑` / `Shift+↓`), and native Herdr mouse drag-select copy.
- **Interactive `/` Search** — global project-wide file/folder search in **FILES**, or in-file text search in **CODE** with `↑` / `↓` (or `n` / `N`) match navigation.
- **Mouse Support & Native Drag-Select Copy** — click to select/open folders and files in **FILES**, and drag-select or double-click text in full-width **CODE** read mode to auto-copy via Herdr.
- **Smart Refocus** — pressing `prefix+y` again while the KAYE panel is already open in the current tab focuses back to it instead of opening a duplicate.

## 1. Install

### Install from GitHub (Recommended)

Requires `cargo` (Rust toolchain) and `kaye` on `PATH`:

```bash
herdr plugin install 1kaye/herdr-kaye
```

*(Note: if you already linked this repo locally with `herdr plugin link`, run `herdr plugin unlink ikaye.kaye` first before installing from GitHub.)*

### Local Development

```bash
cd /home/ikaye/Projects/herdr-kaye
cargo build --release
herdr plugin link /home/ikaye/Projects/herdr-kaye
```

Verify registration:

```bash
herdr plugin list
herdr plugin action list --plugin ikaye.kaye
```

## 2. Configure Keybinding in `~/.config/herdr/config.toml`

Herdr does not auto-bind keys from plugins. Add `[[keys.command]]` to `~/.config/herdr/config.toml` (`prefix+y` does not conflict with default Herdr bindings or `prefix = "ctrl+space"`):

```toml
[[keys.command]]
key = "prefix+y"
type = "plugin_action"
command = "ikaye.kaye.open"
description = "Open KAYE explorer panel"
```

Reload Herdr config without restarting your session:

```bash
herdr server reload-config
```

## 3. Usage & Controls

### Opening & Refocusing the Panel in Herdr

- **`Ctrl+Space` → `y`** (`prefix+y`) — opens the KAYE split panel on the right, or **jumps focus back to the already-open KAYE panel** in the current tab.
- **Switch back & forth between panes** (with your Herdr config):
  - `Ctrl+Alt+Left` / `Ctrl+Alt+Right` — move focus between your terminal and the KAYE pane
  - `Ctrl+Space` → `;` (`last_pane`) — toggle back and forth between the two most recent panes
  - **Mouse click** directly inside either pane

### Inside the KAYE Panel

| Key / Input | Action |
|---|---|
| `↑` / `↓` or `j` / `k` | Move selection (FILES) or move line pointer `▸` (CODE) |
| `←` / `→` or `h` / `l` | Go up / open folder (in FILES), or pan code horizontally left / right (in CODE) |
| `0` | Reset horizontal code scroll to column 0 (in CODE) |
| `Enter` | Open selected folder, or enter full-width CODE read-only mode |
| `Esc` | Clear active search, return from CODE read-only mode to FILES, or close panel (in FILES) |
| `Tab` | Switch focus between **FILES** and **CODE** |
| `/` | Search folder/file names (in FILES) or text inside the file (in CODE); use `↑` / `↓` to move through matches, `Enter` to confirm, `Esc` to clear |
| `Shift+↑` / `Shift+↓` or `n` / `N` | Jump to previous / next `+` / `-` change block (or `n`/`N` for search matches) |
| `a` | Enter starting checkpoint `N` (`1` = initial, latest = `HEAD`) or `all` |
| `f` | Toggle **edited-only** filter |
| `r` | Refresh from KAYE |
| **Left click** | Select item or open selected folder (in FILES) |
| **Mouse drag / double-click** | Native Herdr drag-select / word-select auto-copy (in CODE read mode) |
| **Mouse wheel** | Scroll whichever pane the cursor is over (in FILES) |
| `q` / `Esc` | Close panel |

## 4. Standalone Test

```bash
./target/release/herdr-kaye panel /path/to/project
```

## License

[MIT](LICENSE) © 2026 1kaye
