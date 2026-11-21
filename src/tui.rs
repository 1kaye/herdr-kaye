use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::kaye::{
    CheckpointRange, CodeLine, Entry, FileStatus, KayeRepo, LineKind, kaye_head_hash,
};

// ANSI color codes
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const REVERSE: &str = "\x1b[7m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const BLUE: &str = "\x1b[34m";
const CYAN: &str = "\x1b[36m";
const BG_SEL: &str = "\x1b[48;5;237m";
const BG_BAR: &str = "\x1b[48;5;235m";

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

const POLLIN: i16 = 1;

unsafe extern "C" {
    fn poll(fds: *mut PollFd, nfds: usize, timeout: i32) -> i32;
    fn dup(fd: i32) -> i32;
}

fn open_tty() -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .or_else(|_| {
            let fd = unsafe { dup(0) };
            if fd < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(unsafe { File::from_raw_fd(fd) })
            }
        })
}

struct TerminalGuard {
    tty: File,
    saved_stty: String,
}

impl TerminalGuard {
    fn enter(tty: &File) -> io::Result<Self> {
        let saved = stty_output(tty, &["-g"])?;
        stty_run(tty, &["raw", "-echo"])?;
        let mut out = io::stdout();
        // Alternate screen + hide cursor + SGR mouse clicks & wheel.
        out.write_all(b"\x1b[?1049h\x1b[?25l\x1b[?1000h\x1b[?1006h")?;
        out.flush()?;
        Ok(Self {
            tty: tty.try_clone()?,
            saved_stty: saved,
        })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut out = io::stdout();
        let _ = out.write_all(b"\x1b[?1006l\x1b[?1000l\x1b[?25h\x1b[?1049l");
        let _ = out.flush();
        let _ = stty_run(&self.tty, &[self.saved_stty.trim()]);
    }
}

fn stty_output(tty: &File, args: &[&str]) -> io::Result<String> {
    let dup = tty.try_clone()?;
    let out = Command::new("stty")
        .args(args)
        .stdin(Stdio::from(dup))
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("stty failed"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn stty_run(tty: &File, args: &[&str]) -> io::Result<()> {
    let dup = tty.try_clone()?;
    let status = Command::new("stty")
        .args(args)
        .stdin(Stdio::from(dup))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("stty failed"))
    }
}

fn terminal_size(tty: &File) -> (usize, usize) {
    if let Ok(s) = stty_output(tty, &["size"]) {
        let mut it = s.split_whitespace();
        if let (Some(r), Some(c)) = (it.next(), it.next())
            && let (Ok(rows), Ok(cols)) = (r.parse::<usize>(), c.parse::<usize>())
                && rows >= 8 && cols >= 20 {
                    return (rows, cols);
                }
    }
    (24, 80)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    ShiftUp,
    ShiftDown,
    Left,
    Right,
    Enter,
    Backspace,
    Tab,
    PageUp,
    PageDown,
    Home,
    End,
    Esc,
    Char(char),
    Ctrl(char),
    MouseClick { col: usize, row: usize },
    MouseScrollUp { col: usize },
    MouseScrollDown { col: usize },
    Other,
}

fn poll_readable(fd: i32, timeout_ms: i32) -> bool {
    let mut pfd = PollFd {
        fd,
        events: POLLIN,
        revents: 0,
    };
    let rc = unsafe { poll(&mut pfd as *mut PollFd, 1, timeout_ms) };
    rc > 0 && (pfd.revents & POLLIN) != 0
}

fn read_byte(tty: &mut File) -> io::Result<Option<u8>> {
    let mut b = [0u8; 1];
    match tty.read(&mut b) {
        Ok(0) => Ok(None),
        Ok(_) => Ok(Some(b[0])),
        Err(e) if e.kind() == io::ErrorKind::Interrupted => Ok(Some(0)),
        Err(e) => Err(e),
    }
}

fn read_key(tty: &mut File) -> io::Result<Option<Key>> {
    let Some(b) = read_byte(tty)? else {
        return Ok(None);
    };
    let key = match b {
        0 => Key::Other,
        3 => Key::Ctrl('c'),
        4 => Key::Ctrl('d'),
        9 => Key::Tab,
        10 | 13 => Key::Enter,
        21 => Key::Ctrl('u'),
        8 | 127 => Key::Backspace,
        27 => {
            if !poll_readable(tty.as_raw_fd(), 25) {
                Key::Esc
            } else if let Some(b2) = read_byte(tty)? {
                match b2 {
                    b'[' => read_csi(tty)?,
                    b'O' => match read_byte(tty)? {
                        Some(b'A') => Key::Up,
                        Some(b'B') => Key::Down,
                        Some(b'C') => Key::Right,
                        Some(b'D') => Key::Left,
                        Some(b'H') => Key::Home,
                        Some(b'F') => Key::End,
                        _ => Key::Other,
                    },
                    _ => Key::Other,
                }
            } else {
                Key::Esc
            }
        }
        c if (0x20..=0x7e).contains(&c) => Key::Char(c as char),
        _ => Key::Other,
    };
    Ok(Some(key))
}

fn read_csi(tty: &mut File) -> io::Result<Key> {
    let Some(first) = read_byte(tty)? else {
        return Ok(Key::Esc);
    };
    match first {
        b'A' => Ok(Key::Up),
        b'B' => Ok(Key::Down),
        b'C' => Ok(Key::Right),
        b'D' => Ok(Key::Left),
        b'H' => Ok(Key::Home),
        b'F' => Ok(Key::End),
        b'<' => {
            // SGR mouse: `<btn;col;rowM` (press) or `<btn;col;rowm` (release).
            let mut seq = String::new();
            for _ in 0..32 {
                let Some(ch) = read_byte(tty)? else { break };
                let c = ch as char;
                seq.push(c);
                if c == 'M' || c == 'm' {
                    break;
                }
            }
            Ok(parse_sgr_mouse(&seq))
        }
        b'0'..=b'9' => {
            let mut seq = vec![first];
            for _ in 0..16 {
                let Some(ch) = read_byte(tty)? else { break };
                seq.push(ch);
                if (0x40..=0x7e).contains(&ch) {
                    break;
                }
            }
            Ok(match seq.as_slice() {
                b"1;2A" => Key::ShiftUp,
                b"1;2B" => Key::ShiftDown,
                b"5~" => Key::PageUp,
                b"6~" => Key::PageDown,
                b"1~" | b"7~" => Key::Home,
                b"4~" | b"8~" => Key::End,
                _ => Key::Other,
            })
        }
        _ => Ok(Key::Other),
    }
}

pub fn parse_sgr_mouse(seq: &str) -> Key {
    let Some(body) = seq.strip_suffix('M') else {
        return Key::Other;
    };
    let mut parts = body.split(';');
    let (Some(b), Some(c), Some(r)) = (parts.next(), parts.next(), parts.next()) else {
        return Key::Other;
    };
    let (Ok(btn), Ok(col), Ok(row)) = (b.parse::<u32>(), c.parse::<usize>(), r.parse::<usize>())
    else {
        return Key::Other;
    };
    match btn {
        0 => Key::MouseClick { col, row },
        64 => Key::MouseScrollUp { col },
        65 => Key::MouseScrollDown { col },
        _ => Key::Other,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Explorer,
    Code,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SearchMode {
    Off,
    Typing,
    Locked,
}

enum Row {
    Parent,
    Item(Entry),
}

struct App {
    repo: KayeRepo,
    range: CheckpointRange,
    rel_dir: String,
    rows: Vec<Row>,
    selected: usize,
    list_scroll: usize,
    focus: Focus,
    changed_only: bool,
    code_lines: Vec<CodeLine>,
    code_title: String,
    code_scroll: usize,
    change_anchors: Vec<usize>,
    change_idx: usize,
    search_mode: SearchMode,
    search_target: Focus,
    search_query: String,
    depth_prompt: bool,
    depth_input: String,
    code_matches: Vec<usize>,
    code_match_idx: usize,
}

impl App {
    fn new(start_dir: &Path) -> Self {
        let range = CheckpointRange::Latest;
        let repo = KayeRepo::load(start_dir, range);
        let canon_start = start_dir
            .canonicalize()
            .unwrap_or_else(|_| start_dir.to_path_buf());
        let rel_dir = canon_start
            .strip_prefix(&repo.root)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();

        let mut app = Self {
            repo,
            range,
            rel_dir,
            rows: Vec::new(),
            selected: 0,
            list_scroll: 0,
            focus: Focus::Explorer,
            changed_only: false,
            code_lines: Vec::new(),
            code_title: String::new(),
            code_scroll: 0,
            change_anchors: Vec::new(),
            change_idx: 0,
            search_mode: SearchMode::Off,
            search_target: Focus::Explorer,
            search_query: String::new(),
            depth_prompt: false,
            depth_input: String::new(),
            code_matches: Vec::new(),
            code_match_idx: 0,
        };
        app.rebuild_rows(None);
        app
    }

    fn reload(&mut self) {
        self.reload_inner(false);
    }

    fn reload_preserving_scroll(&mut self) {
        self.reload_inner(true);
    }

    fn reload_inner(&mut self, preserve_scroll: bool) {
        let prev_name = self.selected_name();
        let prev_code_scroll = self.code_scroll;
        let prev_list_scroll = self.list_scroll;
        let target = if self.rel_dir.is_empty() {
            self.repo.root.clone()
        } else {
            self.repo.root.join(&self.rel_dir)
        };
        self.repo = KayeRepo::load(&target, self.range);
        self.rebuild_rows(prev_name.as_deref());
        if preserve_scroll && self.selected_name() == prev_name {
            self.code_scroll = prev_code_scroll.min(self.code_lines.len().saturating_sub(1));
            self.list_scroll = prev_list_scroll.min(self.rows.len().saturating_sub(1));
        }
    }

    fn selected_name(&self) -> Option<String> {
        match self.rows.get(self.selected) {
            Some(Row::Parent) => Some("..".to_string()),
            Some(Row::Item(e)) => Some(e.name.clone()),
            None => None,
        }
    }

    fn is_explorer_search(&self) -> bool {
        self.search_mode != SearchMode::Off
            && self.search_target == Focus::Explorer
            && !self.search_query.is_empty()
    }

    fn is_code_search(&self) -> bool {
        self.search_mode != SearchMode::Off
            && self.search_target == Focus::Code
            && !self.search_query.is_empty()
    }

    fn clear_search(&mut self) {
        let was_explorer = self.is_explorer_search();
        self.search_mode = SearchMode::Off;
        self.search_query.clear();
        self.code_matches.clear();
        self.code_match_idx = 0;
        if was_explorer {
            self.list_scroll = 0;
            self.rebuild_rows(None);
        }
    }

    fn start_search(&mut self) {
        let prev_selected = self.selected_name();
        self.search_target = self.focus;
        self.search_mode = SearchMode::Typing;
        self.search_query.clear();
        self.code_matches.clear();
        self.code_match_idx = 0;
        if self.search_target == Focus::Explorer {
            self.rebuild_rows(prev_selected.as_deref());
        }
    }

    fn update_code_matches(&mut self, page_h: usize) {
        self.code_matches.clear();
        self.code_match_idx = 0;
        let q = self.search_query.to_lowercase();
        if q.is_empty() {
            return;
        }
        for (i, line) in self.code_lines.iter().enumerate() {
            if line.kind != LineKind::Info && line.text.to_lowercase().contains(&q) {
                self.code_matches.push(i);
            }
        }
        if let Some(&first) = self.code_matches.first() {
            self.scroll_code_to(first, page_h);
        }
    }

    fn step_code_match(&mut self, forward: bool, page_h: usize) {
        if self.code_matches.is_empty() {
            return;
        }
        let len = self.code_matches.len();
        self.code_match_idx = if forward {
            (self.code_match_idx + 1) % len
        } else {
            (self.code_match_idx + len - 1) % len
        };
        let target = self.code_matches[self.code_match_idx];
        self.scroll_code_to(target, page_h);
    }

    fn scroll_code_to(&mut self, line_idx: usize, page_h: usize) {
        let max_scroll = self.code_lines.len().saturating_sub(page_h);
        self.code_scroll = line_idx.saturating_sub(4).min(max_scroll);
    }

    fn rebuild_rows(&mut self, select_name: Option<&str>) {
        let mut rows = Vec::new();
        if self.is_explorer_search() {
            // Search globally from project root even when browsing inside a subfolder.
            for e in self
                .repo
                .search_entries("", &self.search_query, self.changed_only)
            {
                rows.push(Row::Item(e));
            }
        } else {
            if !self.rel_dir.is_empty() {
                rows.push(Row::Parent);
            }
            for e in self.repo.list_dir(&self.rel_dir, self.changed_only) {
                rows.push(Row::Item(e));
            }
        }
        self.rows = rows;

        self.selected = select_name
            .and_then(|name| {
                self.rows.iter().position(|r| match r {
                    Row::Parent => name == "..",
                    Row::Item(e) => e.name == name,
                })
            })
            .unwrap_or(0);

        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
        self.load_preview();
    }

    fn load_preview(&mut self) {
        self.code_scroll = 0;
        self.change_anchors.clear();
        self.change_idx = 0;
        if self.search_target == Focus::Code {
            self.code_matches.clear();
            self.code_match_idx = 0;
        }
        match self.rows.get(self.selected) {
            None => {
                self.code_title = "Empty".to_string();
                self.code_lines = vec![info_line(if self.is_explorer_search() {
                    "No matching folders or files."
                } else if self.changed_only {
                    "No edited files here. Press 'f' for all files or ']' for older checkpoints."
                } else {
                    "Folder is empty."
                })];
            }
            Some(Row::Parent) => {
                self.code_title = "../ (parent folder)".to_string();
                self.code_lines = vec![info_line("Press Enter or Left to go up one folder.")];
            }
            Some(Row::Item(e)) if e.is_dir => {
                self.code_title = format!("{}/", e.rel_path);
                let changed = self.repo.changed_files_under(&e.rel_path);
                if changed.is_empty() {
                    self.code_lines = vec![
                        info_line("No edited files in this folder for current checkpoint range."),
                        info_line(
                            "Press Enter or Right to open folder, or ']' for more checkpoints.",
                        ),
                    ];
                } else {
                    let mut lines = vec![info_line(&format!(
                        "Changed files in {}/ ({} files, +{} -{}):",
                        e.rel_path, e.changed_files, e.added, e.deleted
                    ))];
                    lines.push(info_line(""));
                    for (path, ch) in changed {
                        let short = path
                            .strip_prefix(&format!("{}/", e.rel_path))
                            .unwrap_or(path);
                        lines.push(CodeLine {
                            line_no: None,
                            kind: LineKind::Info,
                            text: format!(
                                "SUMMARY\t{}\t{}\t{}\t{}",
                                ch.status.badge(),
                                short,
                                ch.added,
                                ch.deleted
                            ),
                        });
                    }
                    self.code_lines = lines;
                }
            }
            Some(Row::Item(e)) => {
                self.code_title = match e.status {
                    Some(st) => {
                        format!("{} [{}] +{} -{}", e.rel_path, st.badge(), e.added, e.deleted)
                    }
                    None => e.rel_path.clone(),
                };
                let (lines, first_change) = self.repo.file_view(&e.rel_path);
                let mut in_change = false;
                for (i, l) in lines.iter().enumerate() {
                    let is_chg = matches!(l.kind, LineKind::Added | LineKind::Deleted);
                    if is_chg && !in_change {
                        self.change_anchors.push(i);
                    }
                    in_change = is_chg;
                }
                self.code_lines = lines;
                if let Some(first) = first_change {
                    self.code_scroll = first.saturating_sub(4);
                }
            }
        }
    }

    fn jump_change(&mut self, forward: bool, page_h: usize, force_hunk: bool) {
        if !force_hunk && self.is_code_search() {
            self.step_code_match(forward, page_h);
            return;
        }
        if self.change_anchors.is_empty() {
            return;
        }
        let len = self.change_anchors.len();
        self.change_idx = if forward {
            (self.change_idx + 1) % len
        } else {
            (self.change_idx + len - 1) % len
        };
        let target = self.change_anchors[self.change_idx];
        self.scroll_code_to(target, page_h);
    }

    fn enter_selected(&mut self) {
        match self.rows.get(self.selected) {
            Some(Row::Parent) => self.go_up(),
            Some(Row::Item(e)) if e.is_dir => {
                let target_dir = e.rel_path.clone();
                self.search_mode = SearchMode::Off;
                self.search_query.clear();
                self.rel_dir = target_dir;
                self.list_scroll = 0;
                self.rebuild_rows(None);
            }
            Some(Row::Item(e)) => {
                if self.search_target == Focus::Explorer && self.search_mode != SearchMode::Off {
                    let rel_path = e.rel_path.clone();
                    let (parent_dir, file_name) = match rel_path.rsplit_once('/') {
                        Some((p, n)) => (p.to_string(), n.to_string()),
                        None => (String::new(), rel_path),
                    };
                    self.search_mode = SearchMode::Off;
                    self.search_query.clear();
                    self.rel_dir = parent_dir;
                    self.list_scroll = 0;
                    self.rebuild_rows(Some(&file_name));
                }
                self.focus = Focus::Code;
            }
            None => {}
        }
    }

    fn go_up(&mut self) {
        if self.rel_dir.is_empty() {
            return;
        }
        let child_name = self
            .rel_dir
            .rsplit_once('/')
            .map(|(_, c)| c.to_string())
            .unwrap_or_else(|| self.rel_dir.clone());
        self.rel_dir = self
            .rel_dir
            .rsplit_once('/')
            .map(|(p, _)| p.to_string())
            .unwrap_or_default();
        self.list_scroll = 0;
        self.rebuild_rows(Some(&child_name));
    }

    fn move_explorer(&mut self, delta: isize, page_h: usize) {
        if self.rows.is_empty() {
            return;
        }
        let next = (self.selected as isize + delta)
            .clamp(0, self.rows.len().saturating_sub(1) as isize) as usize;
        if next != self.selected {
            self.selected = next;
            self.load_preview();
        }
        if self.selected < self.list_scroll {
            self.list_scroll = self.selected;
        } else if self.selected >= self.list_scroll + page_h {
            self.list_scroll = self.selected + 1 - page_h;
        }
    }

    fn scroll_code(&mut self, delta: isize, page_h: usize) {
        let max_scroll = self.code_lines.len().saturating_sub(page_h);
        self.code_scroll =
            (self.code_scroll as isize + delta).clamp(0, max_scroll as isize) as usize;
    }

    fn move_cursor(&mut self, delta: isize, page_h: usize) {
        match self.focus {
            Focus::Explorer => self.move_explorer(delta, page_h),
            Focus::Code => self.scroll_code(delta, page_h),
        }
    }

    fn handle_mouse_click(&mut self, col: usize, row: usize, rows: usize, cols: usize) {
        let (split, left_w) = layout_widths(cols);
        if row <= 1 || row >= rows {
            return;
        }
        // Sub-header click switches focus.
        if row == 2 {
            if split {
                self.focus = if col <= left_w {
                    Focus::Explorer
                } else {
                    Focus::Code
                };
            } else {
                self.focus = match self.focus {
                    Focus::Explorer => Focus::Code,
                    Focus::Code => Focus::Explorer,
                };
            }
            return;
        }

        let body_idx = row - 3;
        let clicked_explorer = if split {
            col <= left_w
        } else {
            self.focus == Focus::Explorer
        };

        if clicked_explorer {
            self.focus = Focus::Explorer;
            let idx = self.list_scroll + body_idx;
            if idx < self.rows.len() {
                if idx == self.selected {
                    // Second click on selected folder/parent enters it.
                    let is_nav = matches!(
                        self.rows.get(idx),
                        Some(Row::Parent) | Some(Row::Item(Entry { is_dir: true, .. }))
                    );
                    if is_nav {
                        self.enter_selected();
                    }
                } else {
                    self.selected = idx;
                    self.load_preview();
                }
            }
        } else {
            self.focus = Focus::Code;
        }
    }

    fn handle_mouse_scroll(&mut self, col: usize, delta: isize, cols: usize, body_h: usize) {
        let (split, left_w) = layout_widths(cols);
        let over_explorer = if split {
            col <= left_w
        } else {
            self.focus == Focus::Explorer
        };
        if over_explorer {
            self.move_explorer(delta, body_h);
        } else {
            self.scroll_code(delta, body_h);
        }
    }
}

fn layout_widths(cols: usize) -> (bool, usize) {
    let split = cols >= 65;
    let left_w = if split { (cols * 38 / 100).max(24) } else { cols };
    (split, left_w)
}

fn info_line(s: &str) -> CodeLine {
    CodeLine {
        line_no: None,
        kind: LineKind::Info,
        text: s.to_string(),
    }
}

pub fn run(start_dir: &Path) -> io::Result<()> {
    let mut tty = open_tty()?;
    let _guard = TerminalGuard::enter(&tty)?;
    let mut app = App::new(start_dir);
    let mut last_size = (0usize, 0usize);
    let mut needs_render = true;

    loop {
        let (rows, cols) = terminal_size(&tty);
        if (rows, cols) != last_size {
            last_size = (rows, cols);
            needs_render = true;
        }
        let body_h = rows.saturating_sub(3).max(1);
        if needs_render {
            render(&mut app, rows, cols)?;
            needs_render = false;
        }

        // Poll every 500ms so new `kaye save` checkpoints auto-refresh without pressing `r`.
        if !poll_readable(tty.as_raw_fd(), 500) {
            if !app.depth_prompt && app.search_mode != SearchMode::Typing {
                let latest_head = kaye_head_hash(&app.repo.root);
                if latest_head != app.repo.head_hash
                    || (!app.repo.is_kaye_repo && !latest_head.is_empty())
                {
                    app.reload_preserving_scroll();
                    needs_render = true;
                }
            }
            continue;
        }

        let Some(key) = read_key(&mut tty)? else {
            break;
        };
        needs_render = true;

        // Mouse works in both normal and search modes.
        match key {
            Key::MouseClick { col, row } => {
                app.handle_mouse_click(col, row, rows, cols);
                continue;
            }
            Key::MouseScrollUp { col } => {
                app.handle_mouse_scroll(col, -3, cols, body_h);
                continue;
            }
            Key::MouseScrollDown { col } => {
                app.handle_mouse_scroll(col, 3, cols, body_h);
                continue;
            }
            Key::Ctrl('c') => break,
            _ => {}
        }

        if app.depth_prompt {
            match key {
                Key::Esc => {
                    app.depth_prompt = false;
                    app.depth_input.clear();
                }
                Key::Enter => {
                    let raw = app.depth_input.trim().to_ascii_lowercase();
                    app.depth_prompt = false;
                    app.depth_input.clear();
                    if raw == "all" || raw == "a" {
                        app.range = CheckpointRange::All;
                        app.reload();
                    } else if let Ok(n) = raw.parse::<usize>() {
                        let max_cp = app.repo.total_checkpoints.max(1);
                        let clamped = n.clamp(1, max_cp);
                        app.range = if clamped == max_cp {
                            CheckpointRange::Latest
                        } else {
                            CheckpointRange::From(clamped)
                        };
                        app.reload();
                    }
                }
                Key::Backspace => {
                    app.depth_input.pop();
                }
                Key::Char(c) if c.is_ascii_alphanumeric() => {
                    app.depth_input.push(c);
                }
                _ => {}
            }
            continue;
        }

        if app.search_mode == SearchMode::Typing {
            match key {
                Key::Esc => app.clear_search(),
                Key::Enter => {
                    if app.search_target == Focus::Explorer {
                        app.enter_selected();
                    } else {
                        app.search_mode = if app.search_query.is_empty() {
                            SearchMode::Off
                        } else {
                            SearchMode::Locked
                        };
                    }
                }
                Key::Backspace => {
                    app.search_query.pop();
                    if app.search_target == Focus::Explorer {
                        app.list_scroll = 0;
                        app.rebuild_rows(None);
                    } else {
                        app.update_code_matches(body_h);
                    }
                }
                Key::Up => {
                    if app.search_target == Focus::Explorer {
                        app.move_explorer(-1, body_h);
                    } else {
                        app.step_code_match(false, body_h);
                    }
                }
                Key::Down => {
                    if app.search_target == Focus::Explorer {
                        app.move_explorer(1, body_h);
                    } else {
                        app.step_code_match(true, body_h);
                    }
                }
                Key::Char(c) => {
                    app.search_query.push(c);
                    if app.search_target == Focus::Explorer {
                        app.list_scroll = 0;
                        app.rebuild_rows(None);
                    } else {
                        app.update_code_matches(body_h);
                    }
                }
                _ => {}
            }
            continue;
        }

        match key {
            Key::Char('q') => break,
            Key::Esc => {
                if app.search_mode != SearchMode::Off {
                    app.clear_search();
                } else {
                    break;
                }
            }
            Key::Char('/') => app.start_search(),
            Key::Up | Key::Char('k') => app.move_cursor(-1, body_h),
            Key::Down | Key::Char('j') => app.move_cursor(1, body_h),
            Key::PageUp | Key::Ctrl('u') => app.move_cursor(-(body_h as isize / 2).max(1), body_h),
            Key::PageDown | Key::Ctrl('d') => app.move_cursor((body_h as isize / 2).max(1), body_h),
            Key::Home | Key::Char('g') => app.move_cursor(isize::MIN / 2, body_h),
            Key::End | Key::Char('G') => app.move_cursor(isize::MAX / 2, body_h),
            Key::Enter | Key::Right | Key::Char('l') => {
                if app.focus == Focus::Explorer {
                    app.enter_selected();
                }
            }
            Key::Backspace | Key::Left | Key::Char('h') => {
                if app.focus == Focus::Code {
                    app.focus = Focus::Explorer;
                } else {
                    app.go_up();
                }
            }
            Key::Tab => {
                app.focus = match app.focus {
                    Focus::Explorer => Focus::Code,
                    Focus::Code => Focus::Explorer,
                };
            }
            Key::ShiftDown => app.jump_change(true, body_h, true),
            Key::ShiftUp => app.jump_change(false, body_h, true),
            Key::Char('n') => app.jump_change(true, body_h, false),
            Key::Char('N') => app.jump_change(false, body_h, false),
            Key::Char('[') => {
                let max_cp = app.repo.total_checkpoints.max(1);
                let cur = match app.range {
                    CheckpointRange::Latest => max_cp,
                    CheckpointRange::From(n) => n,
                    CheckpointRange::All => 1,
                };
                app.range = CheckpointRange::From(cur.saturating_sub(1).max(1));
                app.reload();
            }
            Key::Char(']') => {
                let max_cp = app.repo.total_checkpoints.max(1);
                let cur = match app.range {
                    CheckpointRange::Latest => max_cp,
                    CheckpointRange::From(n) => n,
                    CheckpointRange::All => 1,
                };
                let next = (cur + 1).min(max_cp);
                app.range = if next >= max_cp {
                    CheckpointRange::Latest
                } else {
                    CheckpointRange::From(next)
                };
                app.reload();
            }
            Key::Char('a') => {
                app.depth_prompt = true;
                app.depth_input.clear();
            }
            Key::Char('f') => {
                app.changed_only = !app.changed_only;
                app.list_scroll = 0;
                app.rebuild_rows(None);
            }
            Key::Char('r') => app.reload(),
            _ => {}
        }
    }
    Ok(())
}

fn render(app: &mut App, rows: usize, cols: usize) -> io::Result<()> {
    let mut buf = String::with_capacity(rows * cols * 2);
    buf.push_str("\x1b[H");

    let body_h = rows.saturating_sub(3).max(1);
    if app.selected < app.list_scroll {
        app.list_scroll = app.selected;
    } else if app.selected >= app.list_scroll + body_h {
        app.list_scroll = app.selected + 1 - body_h;
    }

    // Header bar
    let root_name = app
        .repo
        .root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| app.repo.root.to_string_lossy().into_owned());
    let path_label = if app.rel_dir.is_empty() {
        format!("{root_name}/")
    } else {
        format!("{root_name}/{}/", app.rel_dir)
    };
    let total = app.repo.total_summary();
    let mode_tag = if app.changed_only { " [edited]" } else { "" };
    let max_cp = app.repo.total_checkpoints;
    let range_tag = match app.range {
        CheckpointRange::Latest => format!("cp {max_cp}/{max_cp}"),
        CheckpointRange::From(n) => {
            let clamped = n.clamp(1, max_cp.max(1));
            if clamped >= max_cp {
                format!("cp {max_cp}/{max_cp}")
            } else {
                format!("cp {clamped}..{max_cp}/{max_cp}")
            }
        }
        CheckpointRange::All => format!("cp 1..{max_cp} (all)"),
    };

    let (header_left, header_right_plain, header_right_styled) = if app.repo.is_kaye_repo {
        let msg_part = if app.repo.checkpoint_msg.is_empty() {
            String::new()
        } else {
            format!(" · \"{}\"", app.repo.checkpoint_msg)
        };
        let left = format!(
            " KAYE ({} · {range_tag}{msg_part}) │ {path_label}{mode_tag}",
            app.repo.branch
        );
        let right_plain = format!(
            "{} files  +{} -{} ",
            total.changed_files, total.added, total.deleted
        );
        let right_styled = format!(
            "{DIM}{} files{RESET}{BG_BAR}  {GREEN}+{}{RESET}{BG_BAR} {RED}-{}{RESET}{BG_BAR} ",
            total.changed_files, total.added, total.deleted
        );
        (left, right_plain, right_styled)
    } else {
        let left = format!(" KAYE (no .kaye/) │ {path_label}");
        let right_plain = "run: kaye init ".to_string();
        let right_styled = format!("{YELLOW}run: kaye init {RESET}{BG_BAR}");
        (left, right_plain, right_styled)
    };

    let right_len = header_right_plain.chars().count();
    let left_max = cols.saturating_sub(right_len + 1);
    let left_fit = truncate_chars(&header_left, left_max);
    let pad = cols.saturating_sub(left_fit.chars().count() + right_len);
    buf.push_str(&format!(
        "{BG_BAR}{BOLD}{CYAN}{left_fit}{RESET}{BG_BAR}{}{header_right_styled}{RESET}\x1b[K\r\n",
        " ".repeat(pad)
    ));

    // Sub-header row
    let (split, left_w) = layout_widths(cols);
    let right_w = if split { cols.saturating_sub(left_w + 1) } else { cols };
    let hunk_tag = if app.change_anchors.is_empty() {
        String::new()
    } else {
        format!(" [{}/{}]", app.change_idx + 1, app.change_anchors.len())
    };

    if split {
        let exp_hdr = format!(
            " {}",
            if app.focus == Focus::Explorer {
                "FILES *"
            } else {
                "FILES"
            }
        );
        let code_hdr = format!(
            " {}{}{}",
            if app.focus == Focus::Code { "* " } else { "" },
            app.code_title,
            hunk_tag
        );
        let l_txt = pad_right(&truncate_chars(&exp_hdr, left_w), left_w);
        let r_txt = pad_right(&truncate_chars(&code_hdr, right_w), right_w);
        let l_col = if app.focus == Focus::Explorer { BOLD } else { DIM };
        let r_col = if app.focus == Focus::Code { BOLD } else { DIM };
        buf.push_str(&format!(
            "{l_col}{l_txt}{RESET}{DIM}│{RESET}{r_col}{r_txt}{RESET}\x1b[K\r\n"
        ));
    } else {
        let hdr = match app.focus {
            Focus::Explorer => " FILES (Tab/Click -> Code)".to_string(),
            Focus::Code => format!(" CODE: {}{} (Tab/Click -> Files)", app.code_title, hunk_tag),
        };
        buf.push_str(&format!(
            "{BOLD}{}{RESET}\x1b[K\r\n",
            truncate_chars(&hdr, cols)
        ));
    }

    // Body rows
    for i in 0..body_h {
        if split {
            let left_line = render_explorer_row(app, app.list_scroll + i, left_w);
            let right_line = render_code_row(app, app.code_scroll + i, right_w);
            buf.push_str(&left_line);
            buf.push_str(DIM);
            buf.push('│');
            buf.push_str(RESET);
            buf.push_str(&right_line);
            buf.push_str("\x1b[K\r\n");
        } else {
            let line = match app.focus {
                Focus::Explorer => render_explorer_row(app, app.list_scroll + i, cols),
                Focus::Code => render_code_row(app, app.code_scroll + i, cols),
            };
            buf.push_str(&line);
            buf.push_str("\x1b[K\r\n");
        }
    }

    // Footer bar (or interactive search / checkpoint prompt)
    if app.depth_prompt {
        let max_cp = app.repo.total_checkpoints.max(1);
        let prompt = format!(
            " Checkpoints (1..{max_cp} or 'all'): {}▎   Enter:apply  Esc:cancel",
            app.depth_input
        );
        let fit = pad_right(&truncate_chars(&prompt, cols), cols);
        buf.push_str(&format!("{BG_BAR}{BOLD}{CYAN}{fit}{RESET}\x1b[K"));
    } else if app.search_mode == SearchMode::Typing {
        let prompt = match app.search_target {
            Focus::Explorer => format!(
                " /{}▎   [{} match(es)]  ↑↓:select  Enter:open  Esc:clear",
                app.search_query,
                app.rows.len()
            ),
            Focus::Code => {
                let status = if app.code_matches.is_empty() {
                    "0/0".to_string()
                } else {
                    format!("{}/{}", app.code_match_idx + 1, app.code_matches.len())
                };
                format!(
                    " /{}▎   [{status}]  ↑↓:prev/next match  Enter:keep  Esc:clear",
                    app.search_query
                )
            }
        };
        let fit = pad_right(&truncate_chars(&prompt, cols), cols);
        buf.push_str(&format!("{BG_BAR}{BOLD}{YELLOW}{fit}{RESET}\x1b[K"));
    } else if app.is_code_search() {
        let status = if app.code_matches.is_empty() {
            "0/0".to_string()
        } else {
            format!("{}/{}", app.code_match_idx + 1, app.code_matches.len())
        };
        let footer = format!(
            " Search '{}' [{status}]  n/N:match  /:new  Esc:clear  q:close",
            app.search_query
        );
        let fit = pad_right(&truncate_chars(&footer, cols), cols);
        buf.push_str(&format!("{BG_BAR}{YELLOW}{fit}{RESET}\x1b[K"));
    } else {
        let footer =
            " ↑↓/jk:move  Shift+↑↓:diff  Enter/Click:open  Tab:pane  /:search  a:cp  f:edited  q:close";
        let fit = pad_right(&truncate_chars(footer, cols), cols);
        buf.push_str(&format!("{BG_BAR}{DIM}{fit}{RESET}\x1b[K"));
    }

    let mut stdout = io::stdout();
    stdout.write_all(buf.as_bytes())?;
    stdout.flush()
}

fn render_explorer_row(app: &App, idx: usize, width: usize) -> String {
    let Some(row) = app.rows.get(idx) else {
        return " ".repeat(width);
    };
    let is_sel = idx == app.selected;
    let bg = if is_sel { BG_SEL } else { "" };

    match row {
        Row::Parent => {
            let text = pad_right(&truncate_chars(" ↩ ../", width), width);
            format!("{bg}{CYAN}{text}{RESET}")
        }
        Row::Item(e) => {
            let (badge_plain, badge_styled) = if e.is_dir {
                if e.changed_files > 0 {
                    ("▸*", format!("{YELLOW}▸*{RESET}{bg}"))
                } else {
                    ("▸ ", format!("{BLUE}▸ {RESET}{bg}"))
                }
            } else {
                match e.status {
                    Some(FileStatus::Modified) => ("M ", format!("{YELLOW}M {RESET}{bg}")),
                    Some(FileStatus::Added) => ("A ", format!("{GREEN}A {RESET}{bg}")),
                    Some(FileStatus::Deleted) => ("D ", format!("{RED}D {RESET}{bg}")),
                    Some(FileStatus::Renamed) => ("R ", format!("{CYAN}R {RESET}{bg}")),
                    None => ("  ", "  ".to_string()),
                }
            };

            let (stat_plain, stat_styled) = format_stats(
                e.added,
                e.deleted,
                e.status.is_some() || e.changed_files > 0,
                bg,
            );
            let stat_w = stat_plain.chars().count();
            let prefix_w = 1 + badge_plain.chars().count();
            let name_max = width.saturating_sub(prefix_w + stat_w + 1);
            let raw_name = if e.is_dir {
                format!("{}/", e.name)
            } else {
                e.name.clone()
            };
            let name_fit = truncate_chars(&raw_name, name_max);
            let name_w = name_fit.chars().count();
            let gap = width.saturating_sub(prefix_w + name_w + stat_w);

            let name_color = if e.is_dir {
                if e.changed_files > 0 { YELLOW } else { BLUE }
            } else {
                match e.status {
                    Some(FileStatus::Deleted) => RED,
                    Some(FileStatus::Added) => GREEN,
                    Some(FileStatus::Modified) => YELLOW,
                    Some(FileStatus::Renamed) => CYAN,
                    None => "",
                }
            };

            format!(
                "{bg} {badge_styled}{name_color}{name_fit}{RESET}{bg}{}{stat_styled}{RESET}",
                " ".repeat(gap)
            )
        }
    }
}

fn format_stats(added: usize, deleted: usize, has_change: bool, bg: &str) -> (String, String) {
    if !has_change || (added == 0 && deleted == 0) {
        return (String::new(), String::new());
    }
    let mut plain = String::new();
    let mut styled = String::new();
    if added > 0 {
        plain.push_str(&format!(" +{added}"));
        styled.push_str(&format!(" {GREEN}+{added}{RESET}{bg}"));
    }
    if deleted > 0 {
        plain.push_str(&format!(" -{deleted}"));
        styled.push_str(&format!(" {RED}-{deleted}{RESET}{bg}"));
    }
    plain.push(' ');
    styled.push(' ');
    (plain, styled)
}

fn render_code_row(app: &App, idx: usize, width: usize) -> String {
    let Some(line) = app.code_lines.get(idx) else {
        return " ".repeat(width);
    };

    if line.kind == LineKind::Info {
        if let Some(rest) = line.text.strip_prefix("SUMMARY\t") {
            let mut parts = rest.splitn(4, '\t');
            let badge = parts.next().unwrap_or("M");
            let path = parts.next().unwrap_or("");
            let add: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let del: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let (stat_plain, stat_styled) = format_stats(add, del, true, "");
            let stat_w = stat_plain.chars().count();
            let name_max = width.saturating_sub(4 + stat_w);
            let name_fit = truncate_chars(path, name_max);
            let gap = width.saturating_sub(3 + name_fit.chars().count() + stat_w);
            let b_col = match badge {
                "A" => GREEN,
                "D" => RED,
                "R" => CYAN,
                _ => YELLOW,
            };
            return format!(
                " {b_col}{badge}{RESET} {name_fit}{}{stat_styled}",
                " ".repeat(gap)
            );
        }
        let fit = pad_right(&truncate_chars(&format!(" {}", line.text), width), width);
        return format!("{DIM}{fit}{RESET}");
    }

    let is_active_match = app.is_code_search()
        && app.code_matches.get(app.code_match_idx).copied() == Some(idx);
    let row_bg = if is_active_match { BG_SEL } else { "" };

    let gutter_w = 7usize;
    let body_w = width.saturating_sub(gutter_w);
    let clean = line.text.replace('\t', "    ");
    let query = if app.is_code_search() {
        app.search_query.as_str()
    } else {
        ""
    };

    match line.kind {
        LineKind::Added => {
            let num = line
                .line_no
                .map(|n| format!("{n:>4}"))
                .unwrap_or_else(|| "    ".to_string());
            let raw = pad_right(&truncate_chars(&format!("+{clean}"), body_w), body_w);
            let body = highlight_query(&raw, query, GREEN, row_bg);
            format!("{row_bg}{GREEN}{num} │ {body}{RESET}")
        }
        LineKind::Deleted => {
            let raw = pad_right(&truncate_chars(&format!("-{clean}"), body_w), body_w);
            let body = highlight_query(&raw, query, RED, row_bg);
            format!("{row_bg}{RED}   - │ {body}{RESET}")
        }
        LineKind::Context => {
            let num = line
                .line_no
                .map(|n| format!("{n:>4}"))
                .unwrap_or_else(|| "    ".to_string());
            let raw = pad_right(&truncate_chars(&format!(" {clean}"), body_w), body_w);
            let body = highlight_query(&raw, query, "", row_bg);
            format!("{row_bg}{DIM}{num} │{RESET}{row_bg} {body}{RESET}")
        }
        LineKind::Info => unreachable!(),
    }
}

// Highlight case-insensitive occurrences of `query` on already-truncated text.
fn highlight_query(text: &str, query: &str, base_fg: &str, row_bg: &str) -> String {
    if query.is_empty() {
        return text.to_string();
    }
    let text_chars: Vec<char> = text.chars().collect();
    let lower_text: Vec<char> = text_chars
        .iter()
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let lower_q: Vec<char> = query.chars().map(|c| c.to_ascii_lowercase()).collect();
    let qlen = lower_q.len();
    if qlen == 0 || qlen > lower_text.len() {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len() + 32);
    let mut i = 0;
    while i < text_chars.len() {
        if i + qlen <= lower_text.len() && lower_text[i..i + qlen] == lower_q[..] {
            out.push_str(REVERSE);
            out.push_str(YELLOW);
            for &ch in &text_chars[i..i + qlen] {
                out.push(ch);
            }
            out.push_str(RESET);
            out.push_str(row_bg);
            out.push_str(base_fg);
            i += qlen;
        } else {
            out.push(text_chars[i]);
            i += 1;
        }
    }
    out
}

fn truncate_chars(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max {
        s.to_string()
    } else if max == 1 {
        "…".to_string()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}

fn pad_right(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - len))
    }
}
