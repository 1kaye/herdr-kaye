use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
}

impl FileStatus {
    pub fn badge(self) -> &'static str {
        match self {
            Self::Modified => "M",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Renamed => "R",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HunkLine {
    Ctx(String),
    Add(String),
    Del(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub new_start: usize,
    pub lines: Vec<HunkLine>,
}

#[derive(Debug, Clone)]
pub struct FileChange {
    pub status: FileStatus,
    pub added: usize,
    pub deleted: usize,
    pub hunks: Vec<Hunk>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DirSummary {
    pub changed_files: usize,
    pub added: usize,
    pub deleted: usize,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub rel_path: String,
    pub is_dir: bool,
    pub status: Option<FileStatus>,
    pub added: usize,
    pub deleted: usize,
    pub changed_files: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Deleted,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeLine {
    pub line_no: Option<usize>,
    pub kind: LineKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointRange {
    Latest,
    From(usize),
    All,
}

impl CheckpointRange {
    pub fn to_what_change_arg(self, total: usize) -> String {
        match self {
            Self::Latest => "1".to_string(),
            Self::From(cp) => {
                let max_cp = total.max(1);
                let clamped = cp.clamp(1, max_cp);
                let count = max_cp.saturating_sub(clamped) + 1;
                count.to_string()
            }
            Self::All => "all".to_string(),
        }
    }
}

pub struct KayeRepo {
    pub root: PathBuf,
    pub is_kaye_repo: bool,
    pub branch: String,
    pub head_hash: String,
    pub total_checkpoints: usize,
    pub checkpoint_msg: String,
    pub changes: BTreeMap<String, FileChange>,
    pub dir_summaries: BTreeMap<String, DirSummary>,
}

impl KayeRepo {
    pub fn load(start_dir: &Path, range: CheckpointRange) -> Self {
        let fallback_root = start_dir
            .canonicalize()
            .unwrap_or_else(|_| start_dir.to_path_buf());

        let Some(root_bytes) = run_kaye(start_dir, &["rev-parse", "--show-toplevel"]) else {
            return Self {
                root: fallback_root,
                is_kaye_repo: false,
                branch: String::new(),
                head_hash: String::new(),
                total_checkpoints: 0,
                checkpoint_msg: String::new(),
                changes: BTreeMap::new(),
                dir_summaries: BTreeMap::new(),
            };
        };

        let root_str = String::from_utf8_lossy(&root_bytes).trim().to_string();
        let root = if root_str.is_empty() {
            fallback_root
        } else {
            PathBuf::from(root_str)
        };

        let branch = run_kaye(&root, &["rev-parse", "--abbrev-ref", "HEAD"])
            .map(|b| String::from_utf8_lossy(&b).trim().to_string())
            .filter(|s| !s.is_empty() && s != "HEAD")
            .unwrap_or_else(|| "init".to_string());

        let head_hash = kaye_head_hash(&root);

        let total_checkpoints = run_kaye(&root, &["rev-list", "--count", "HEAD"])
            .and_then(|b| String::from_utf8_lossy(&b).trim().parse::<usize>().ok())
            .unwrap_or(0);

        let depth_arg = range.to_what_change_arg(total_checkpoints);
        let raw = run_kaye(&root, &["what", "change", &depth_arg])
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();

        let (checkpoint_msg, changes) = parse_what_change(&raw);
        let dir_summaries = aggregate_dirs(&changes);

        Self {
            root,
            is_kaye_repo: true,
            branch,
            head_hash,
            total_checkpoints,
            checkpoint_msg,
            changes,
            dir_summaries,
        }
    }

    pub fn total_summary(&self) -> DirSummary {
        self.dir_summaries.get("").copied().unwrap_or_default()
    }

    pub fn list_dir(&self, rel_dir: &str, changed_only: bool) -> Vec<Entry> {
        let abs_dir = if rel_dir.is_empty() {
            self.root.clone()
        } else {
            self.root.join(rel_dir)
        };

        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut entries = Vec::new();

        if let Ok(rd) = fs::read_dir(&abs_dir) {
            for item in rd.flatten() {
                let name = item.file_name().to_string_lossy().into_owned();
                if name == ".git" || name == ".kaye" {
                    continue;
                }
                let is_dir = item.file_type().map(|t| t.is_dir()).unwrap_or(false);
                let rel_path = join_rel(rel_dir, &name);
                seen.insert(name.clone());

                if is_dir {
                    let sum = self.dir_summaries.get(&rel_path).copied().unwrap_or_default();
                    if changed_only && sum.changed_files == 0 {
                        continue;
                    }
                    entries.push(Entry {
                        name,
                        rel_path,
                        is_dir: true,
                        status: None,
                        added: sum.added,
                        deleted: sum.deleted,
                        changed_files: sum.changed_files,
                    });
                } else {
                    let ch = self.changes.get(&rel_path);
                    if changed_only && ch.is_none() {
                        continue;
                    }
                    entries.push(Entry {
                        name,
                        rel_path,
                        is_dir: false,
                        status: ch.map(|c| c.status),
                        added: ch.map(|c| c.added).unwrap_or(0),
                        deleted: ch.map(|c| c.deleted).unwrap_or(0),
                        changed_files: usize::from(ch.is_some()),
                    });
                }
            }
        }

        // Include deleted files (and directories containing only deleted files) that no longer exist on disk.
        let prefix = if rel_dir.is_empty() {
            String::new()
        } else {
            format!("{rel_dir}/")
        };

        for (path, ch) in &self.changes {
            let Some(rest) = path.strip_prefix(&prefix) else {
                continue;
            };
            if rest.is_empty() {
                continue;
            }
            if let Some((dir_name, _)) = rest.split_once('/') {
                if seen.insert(dir_name.to_string()) {
                    let dir_rel = join_rel(rel_dir, dir_name);
                    let sum = self.dir_summaries.get(&dir_rel).copied().unwrap_or_default();
                    entries.push(Entry {
                        name: dir_name.to_string(),
                        rel_path: dir_rel,
                        is_dir: true,
                        status: None,
                        added: sum.added,
                        deleted: sum.deleted,
                        changed_files: sum.changed_files,
                    });
                }
            } else if seen.insert(rest.to_string()) {
                entries.push(Entry {
                    name: rest.to_string(),
                    rel_path: path.clone(),
                    is_dir: false,
                    status: Some(ch.status),
                    added: ch.added,
                    deleted: ch.deleted,
                    changed_files: 1,
                });
            }
        }

        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        entries
    }

    // Recursive case-insensitive substring search for folder/file names under `rel_dir`.
    pub fn search_entries(&self, rel_dir: &str, query: &str, changed_only: bool) -> Vec<Entry> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return self.list_dir(rel_dir, changed_only);
        }

        let start_abs = if rel_dir.is_empty() {
            self.root.clone()
        } else {
            self.root.join(rel_dir)
        };
        let prefix = if rel_dir.is_empty() {
            String::new()
        } else {
            format!("{rel_dir}/")
        };

        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut results = Vec::new();
        // shortcut: cap walk at 5000 entries to stay instant in huge repos; upgrade to async walk if needed.
        let mut visited = 0usize;
        let mut stack = vec![(start_abs, rel_dir.to_string())];

        while let Some((abs_dir, cur_rel)) = stack.pop() {
            let Ok(rd) = fs::read_dir(&abs_dir) else { continue };
            for item in rd.flatten() {
                visited += 1;
                if visited > 5000 {
                    break;
                }
                let name = item.file_name().to_string_lossy().into_owned();
                if name == ".git" || name == ".kaye" {
                    continue;
                }
                let is_dir = item.file_type().map(|t| t.is_dir()).unwrap_or(false);
                let item_rel = join_rel(&cur_rel, &name);
                let display = item_rel.strip_prefix(&prefix).unwrap_or(&item_rel).to_string();

                if is_dir {
                    stack.push((abs_dir.join(&name), item_rel.clone()));
                    let sum = self.dir_summaries.get(&item_rel).copied().unwrap_or_default();
                    if changed_only && sum.changed_files == 0 {
                        continue;
                    }
                    if display.to_lowercase().contains(&q) && seen.insert(item_rel.clone()) {
                        results.push(Entry {
                            name: display,
                            rel_path: item_rel,
                            is_dir: true,
                            status: None,
                            added: sum.added,
                            deleted: sum.deleted,
                            changed_files: sum.changed_files,
                        });
                    }
                } else {
                    let ch = self.changes.get(&item_rel);
                    if changed_only && ch.is_none() {
                        continue;
                    }
                    if display.to_lowercase().contains(&q) && seen.insert(item_rel.clone()) {
                        results.push(Entry {
                            name: display,
                            rel_path: item_rel,
                            is_dir: false,
                            status: ch.map(|c| c.status),
                            added: ch.map(|c| c.added).unwrap_or(0),
                            deleted: ch.map(|c| c.deleted).unwrap_or(0),
                            changed_files: usize::from(ch.is_some()),
                        });
                    }
                }
            }
            if visited > 5000 {
                break;
            }
        }

        // Also match deleted or unvisited changed files from `kaye what change`.
        for (path, ch) in &self.changes {
            let Some(display) = path.strip_prefix(&prefix) else {
                continue;
            };
            if display.to_lowercase().contains(&q) && seen.insert(path.clone()) {
                results.push(Entry {
                    name: display.to_string(),
                    rel_path: path.clone(),
                    is_dir: false,
                    status: Some(ch.status),
                    added: ch.added,
                    deleted: ch.deleted,
                    changed_files: 1,
                });
            }
        }

        results.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        results
    }

    pub fn changed_files_under(&self, rel_dir: &str) -> Vec<(&str, &FileChange)> {
        let prefix = if rel_dir.is_empty() {
            String::new()
        } else {
            format!("{rel_dir}/")
        };
        self.changes
            .iter()
            .filter(|(p, _)| prefix.is_empty() || p.starts_with(&prefix))
            .map(|(p, c)| (p.as_str(), c))
            .collect()
    }

    // Returns full file code with inline Added (+) / Deleted (-) highlights and index of first change.
    pub fn file_view(&self, rel_path: &str) -> (Vec<CodeLine>, Option<usize>) {
        let change = self.changes.get(rel_path);
        let abs_path = self.root.join(rel_path);

        match fs::read(&abs_path) {
            Ok(bytes) => {
                let sample = &bytes[..bytes.len().min(8192)];
                if sample.contains(&0) {
                    return (
                        vec![CodeLine {
                            line_no: None,
                            kind: LineKind::Info,
                            text: "(binary file)".into(),
                        }],
                        None,
                    );
                }
                let text = String::from_utf8_lossy(&bytes);
                let disk_lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();

                let Some(ch) = change else {
                    let lines = disk_lines
                        .into_iter()
                        .enumerate()
                        .map(|(i, text)| CodeLine {
                            line_no: Some(i + 1),
                            kind: LineKind::Context,
                            text,
                        })
                        .collect();
                    return (lines, None);
                };

                if ch.status == FileStatus::Added {
                    let lines = disk_lines
                        .into_iter()
                        .enumerate()
                        .map(|(i, text)| CodeLine {
                            line_no: Some(i + 1),
                            kind: LineKind::Added,
                            text,
                        })
                        .collect();
                    return (lines, Some(0));
                }

                let lines = build_full_code_view(&disk_lines, &ch.hunks);
                let first_change = lines
                    .iter()
                    .position(|l| matches!(l.kind, LineKind::Added | LineKind::Deleted));
                (lines, first_change)
            }
            Err(_) => {
                // File no longer on disk (e.g. [DELETED] in checkpoint).
                if let Some(ch) = change {
                    let mut lines = Vec::new();
                    for h in &ch.hunks {
                        for hl in &h.lines {
                            if let HunkLine::Del(t) = hl {
                                lines.push(CodeLine {
                                    line_no: None,
                                    kind: LineKind::Deleted,
                                    text: t.clone(),
                                });
                            }
                        }
                    }
                    if !lines.is_empty() {
                        return (lines, Some(0));
                    }
                }
                (
                    vec![CodeLine {
                        line_no: None,
                        kind: LineKind::Info,
                        text: "(file not found on disk)".into(),
                    }],
                    None,
                )
            }
        }
    }
}

pub fn kaye_head_hash(cwd: &Path) -> String {
    run_kaye(cwd, &["rev-parse", "HEAD"])
        .map(|b| String::from_utf8_lossy(&b).trim().to_string())
        .unwrap_or_default()
}

fn run_kaye(cwd: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("kaye").args(args).current_dir(cwd).output().ok()?;
    out.status.success().then_some(out.stdout)
}

fn join_rel(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn parse_what_change(raw: &str) -> (String, BTreeMap<String, FileChange>) {
    let clean = strip_ansi(raw);
    let mut checkpoint_msg = String::new();
    let mut files: BTreeMap<String, FileChange> = BTreeMap::new();
    let mut cur_file: Option<String> = None;
    let mut cur_hunk: Option<Hunk> = None;

    let flush_hunk = |files: &mut BTreeMap<String, FileChange>,
                      cur_file: &Option<String>,
                      cur_hunk: &mut Option<Hunk>| {
        if let (Some(path), Some(hunk)) = (cur_file, cur_hunk.take()) {
            if let Some(fc) = files.get_mut(path) {
                fc.hunks.push(hunk);
            }
        }
    };

    for raw_line in clean.lines() {
        let line = raw_line.trim_end_matches('\r');

        // Checkpoint divider / header: `[hash] message (time ago)`
        if line.starts_with("====") {
            flush_hunk(&mut files, &cur_file, &mut cur_hunk);
            cur_file = None;
            continue;
        }
        if line.starts_with('[') && !line.starts_with("  [") {
            flush_hunk(&mut files, &cur_file, &mut cur_hunk);
            cur_file = None;
            checkpoint_msg = extract_checkpoint_message(line);
            continue;
        }

        // File header: `  [MODIFIED] path` / `  [NEW] path` / `  [DELETED] path`
        if let Some(rest) = line.strip_prefix("  [") {
            if let Some((tag, path_raw)) = rest.split_once("] ") {
                flush_hunk(&mut files, &cur_file, &mut cur_hunk);
                let status = match tag {
                    "NEW" | "ADDED" => FileStatus::Added,
                    "DELETED" => FileStatus::Deleted,
                    "RENAMED" => FileStatus::Renamed,
                    _ => FileStatus::Modified,
                };
                // Renamed paths may appear as `old -> new`; track the current path.
                let path = path_raw
                    .rsplit_once(" -> ")
                    .map(|(_, new_p)| new_p)
                    .unwrap_or(path_raw)
                    .trim()
                    .to_string();
                let entry = files.entry(path.clone()).or_insert_with(|| FileChange {
                    status,
                    added: 0,
                    deleted: 0,
                    hunks: Vec::new(),
                });
                entry.status = status;
                cur_file = Some(path);
                continue;
            }
        }

        // Hunk header: `    @@ -old,len +new,len @@ ...`
        if let Some(rest) = line.strip_prefix("    @@ ") {
            flush_hunk(&mut files, &cur_file, &mut cur_hunk);
            if let Some(new_start) = parse_hunk_new_start(rest) {
                cur_hunk = Some(Hunk {
                    new_start,
                    lines: Vec::new(),
                });
            }
            continue;
        }

        // Hunk body lines (indented by 4 spaces by `kaye what change`)
        if let (Some(path), Some(hunk)) = (&cur_file, &mut cur_hunk) {
            if let Some(body) = line.strip_prefix("    ") {
                if let Some(added) = body.strip_prefix('+') {
                    hunk.lines.push(HunkLine::Add(added.to_string()));
                    if let Some(fc) = files.get_mut(path) {
                        fc.added += 1;
                    }
                } else if let Some(deleted) = body.strip_prefix('-') {
                    hunk.lines.push(HunkLine::Del(deleted.to_string()));
                    if let Some(fc) = files.get_mut(path) {
                        fc.deleted += 1;
                    }
                } else if let Some(ctx) = body.strip_prefix(' ') {
                    hunk.lines.push(HunkLine::Ctx(ctx.to_string()));
                } else if body.is_empty() {
                    hunk.lines.push(HunkLine::Ctx(String::new()));
                }
            }
        }
    }

    flush_hunk(&mut files, &cur_file, &mut cur_hunk);
    (checkpoint_msg, files)
}

fn extract_checkpoint_message(header: &str) -> String {
    let rest = header
        .split_once("] ")
        .map(|(_, r)| r)
        .unwrap_or(header)
        .trim();
    // Strip trailing ` (<relative time>)` added by `kaye what change`.
    if let Some((msg, time_part)) = rest.rsplit_once(" (") {
        if time_part.ends_with(')') {
            return msg.trim().to_string();
        }
    }
    rest.to_string()
}

fn parse_hunk_new_start(rest: &str) -> Option<usize> {
    let plus_pos = rest.find('+')?;
    let after_plus = &rest[plus_pos + 1..];
    let digits: String = after_plus
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse::<usize>().ok()
}

// Overlay parsed hunks onto the full current file so unchanged code stays intact
// while added lines are marked green (+) and deleted lines appear inline in red (-).
pub fn build_full_code_view(disk_lines: &[String], hunks: &[Hunk]) -> Vec<CodeLine> {
    let mut added_mask = vec![false; disk_lines.len()];
    let mut deletions_before: BTreeMap<usize, Vec<String>> = BTreeMap::new();

    for hunk in hunks {
        let mut cur_idx = find_hunk_anchor(disk_lines, hunk);
        for hl in &hunk.lines {
            match hl {
                HunkLine::Ctx(_) => {
                    if cur_idx < disk_lines.len() {
                        cur_idx += 1;
                    }
                }
                HunkLine::Add(_) => {
                    if cur_idx < disk_lines.len() {
                        added_mask[cur_idx] = true;
                        cur_idx += 1;
                    }
                }
                HunkLine::Del(text) => {
                    let slot = cur_idx.min(disk_lines.len());
                    deletions_before.entry(slot).or_default().push(text.clone());
                }
            }
        }
    }

    let mut out = Vec::with_capacity(disk_lines.len() + deletions_before.len());
    for (idx, line) in disk_lines.iter().enumerate() {
        if let Some(dels) = deletions_before.remove(&idx) {
            for d in dels {
                out.push(CodeLine {
                    line_no: None,
                    kind: LineKind::Deleted,
                    text: d,
                });
            }
        }
        out.push(CodeLine {
            line_no: Some(idx + 1),
            kind: if added_mask[idx] {
                LineKind::Added
            } else {
                LineKind::Context
            },
            text: line.clone(),
        });
    }

    // Trailing deletions at EOF.
    for (_, dels) in deletions_before {
        for d in dels {
            out.push(CodeLine {
                line_no: None,
                kind: LineKind::Deleted,
                text: d,
            });
        }
    }

    out
}

fn find_hunk_anchor(disk_lines: &[String], hunk: &Hunk) -> usize {
    let expected = hunk.new_start.saturating_sub(1).min(disk_lines.len());
    // Verify expected position against the first non-deleted line in the hunk;
    // if shifted (e.g. older checkpoint in `what change N`), search nearby.
    let probe: Vec<&str> = hunk
        .lines
        .iter()
        .filter_map(|l| match l {
            HunkLine::Ctx(s) | HunkLine::Add(s) => Some(s.as_str()),
            HunkLine::Del(_) => None,
        })
        .take(3)
        .collect();

    if probe.is_empty() || matches_at(disk_lines, expected, &probe) {
        return expected;
    }

    for delta in 1..=60 {
        if let Some(pos) = expected.checked_sub(delta) {
            if matches_at(disk_lines, pos, &probe) {
                return pos;
            }
        }
        if expected + delta < disk_lines.len() && matches_at(disk_lines, expected + delta, &probe) {
            return expected + delta;
        }
    }
    expected
}

fn matches_at(disk_lines: &[String], start: usize, probe: &[&str]) -> bool {
    if start + probe.len() > disk_lines.len() {
        return false;
    }
    probe
        .iter()
        .enumerate()
        .all(|(i, p)| disk_lines[start + i] == *p)
}

pub fn aggregate_dirs(changes: &BTreeMap<String, FileChange>) -> BTreeMap<String, DirSummary> {
    let mut dirs: BTreeMap<String, DirSummary> = BTreeMap::new();
    for (path, ch) in changes {
        let root_sum = dirs.entry(String::new()).or_default();
        root_sum.changed_files += 1;
        root_sum.added += ch.added;
        root_sum.deleted += ch.deleted;

        let mut cur = path.as_str();
        while let Some((parent, _)) = cur.rsplit_once('/') {
            let entry = dirs.entry(parent.to_string()).or_default();
            entry.changed_files += 1;
            entry.added += ch.added;
            entry.deleted += ch.deleted;
            cur = parent;
        }
    }
    dirs
}
