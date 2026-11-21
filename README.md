# herdr-kaye

A **Herdr (`>= 0.9.5`)** plugin for **KAYE** (local shadow-git VCS): interactive folder explorer with green `+` / red `-` change indicators powered by `kaye what change`, plus a full-code viewer with inline added/deleted highlights, search, and mouse support.

## Features

- **Powered by `kaye what change`** — works immediately after `kaye save` checkpoints (default: latest checkpoint, expandable to last `N` or `all`).
- **Folder & File Explorer** — navigate into folders (`▸*` marks folders with changes and aggregated `+N -M` counts) and see file badges (`M`, `A`, `D`, `R`).
- **Full-Code Inline View** — displays the entire file with line numbers, highlighting added lines in green (`+`) and deleted lines inline in red (`-`), auto-scrolling to the first change.
- **Interactive `/` Search** — search file/folder names in **FILES**, or search text inside **CODE** and cycle through matches with `↑` / `↓` (or `n` / `N`).
- **Mouse Support** — click to focus panes, select/open folders and files, and scroll with the mouse wheel.
- **Smart Refocus** — pressing `prefix+y` again while a KAYE split pane is already open in the current tab focuses back to it instead of opening a duplicate.

## 1. Build & Install (Local Link)

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

## 2. Configure Keybindings in `~/.config/herdr/config.toml`

`prefix+y` (**y** for ka**y**e) does not conflict with default Herdr bindings or tmux-style configs (`prefix = "ctrl+space"`):

```toml
# Open (or refocus) KAYE panel as a right split pane
[[keys.command]]
key = "prefix+y"
type = "plugin_action"
command = "ikaye.kaye.open-split"
description = "Open KAYE explorer split"

# Optional: open KAYE as an 85% x 85% modal popup
[[keys.command]]
key = "prefix+shift+y"
type = "plugin_action"
command = "ikaye.kaye.open"
description = "Open KAYE explorer popup"
```

Reload Herdr config without restarting your session:

```bash
herdr server reload-config
```

## 3. Usage & Controls

### Opening & Refocusing the Panel in Herdr

- **`Ctrl+Space` → `y`** (`prefix+y`) — opens the KAYE panel on the right, or **jumps focus back to the already-open KAYE panel** in the current tab.
- **`Ctrl+Space` → `Shift+Y`** — opens KAYE as a floating popup (`85% x 85%`).
- **Switch back & forth between panes** (with your Herdr config):
  - `Ctrl+Alt+Left` / `Ctrl+Alt+Right` — move focus between your terminal and the KAYE pane
  - `Ctrl+Space` → `;` (`last_pane`) — toggle back and forth between the two most recent panes
  - **Mouse click** directly inside either pane

### Inside the KAYE Panel

| Key / Input | Action |
|---|---|
| `↑` / `↓` or `j` / `k` | Move selection (FILES) or scroll code (CODE) |
| `Enter` / `→` / `l` | Open selected folder, or move focus into CODE |
| `Backspace` / `←` / `h` | Go up to parent folder, or return from CODE to FILES |
| `Tab` | Switch focus between **FILES** and **CODE** |
| `/` | Search folder/file names (in FILES) or text inside the file (in CODE); use `↑` / `↓` to move through matches, `Enter` to confirm, `Esc` to clear |
| `Shift+↑` / `Shift+↓` or `n` / `N` | Jump to previous / next `+` / `-` change block (or `n`/`N` for search matches) |
| `[` / `]` / `a` | Decrease / increase checkpoint depth, or press `a` to enter a number `N` or `all` |
| `f` | Toggle **edited-only** filter |
| `r` | Refresh from KAYE |
| **Left click** | Focus pane, select item, or click selected folder again to open it |
| **Mouse wheel** | Scroll whichever pane the cursor is over |
| `q` / `Esc` | Close panel |

## 4. Standalone Test

```bash
./target/release/herdr-kaye panel /path/to/project
```
