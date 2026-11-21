mod kaye;
mod tui;

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const PLUGIN_ID: &str = "ikaye.kaye";
const ENTRYPOINT_ID: &str = "panel";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("open") => open_herdr_panel(),
        Some("panel") => {
            let dir = resolve_panel_dir(args.get(1).map(String::as_str));
            run_panel(&dir)
        }
        Some("--help" | "-h") => {
            eprintln!("Usage:\n  herdr-kaye open\n  herdr-kaye panel [path]");
            ExitCode::SUCCESS
        }
        Some(path) => run_panel(&PathBuf::from(path)),
        None => {
            let dir = resolve_panel_dir(None);
            run_panel(&dir)
        }
    }
}

fn run_panel(dir: &Path) -> ExitCode {
    match tui::run(dir) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("herdr-kaye error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn open_herdr_panel() -> ExitCode {
    let herdr_bin = env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".to_string());

    // Refocus existing KAYE panel in the current tab instead of opening a duplicate.
    if let Some(pane_id) = find_existing_kaye_pane(&herdr_bin)
        && let Ok(st) = Command::new(&herdr_bin)
            .args(["plugin", "pane", "focus", &pane_id])
            .status()
        && st.success()
    {
        return ExitCode::SUCCESS;
    }

    let cwd = resolve_active_pane_cwd(&herdr_bin);
    let cwd_str = cwd.to_string_lossy().into_owned();

    let mut cmd = Command::new(&herdr_bin);
    cmd.args([
        "plugin",
        "pane",
        "open",
        "--plugin",
        PLUGIN_ID,
        "--entrypoint",
        ENTRYPOINT_ID,
        "--placement",
        "split",
        "--direction",
        "right",
        // Keep plugin root as process cwd so relative manifest command always resolves;
        // pass the active pane's project path via HERDR_KAYE_CWD.
        "--env",
        &format!("HERDR_KAYE_CWD={cwd_str}"),
        "--focus",
    ]);

    if let Ok(pane_id) = env::var("HERDR_PANE_ID")
        && !pane_id.is_empty()
    {
        cmd.args(["--target-pane", &pane_id]);
    }

    match cmd.status() {
        Ok(st) if st.success() => ExitCode::SUCCESS,
        Ok(st) => {
            eprintln!("herdr plugin pane open exited with {st}");
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("failed to invoke {herdr_bin}: {err}");
            ExitCode::FAILURE
        }
    }
}

fn resolve_panel_dir(explicit: Option<&str>) -> PathBuf {
    if let Some(p) = explicit {
        return PathBuf::from(p);
    }
    for key in ["HERDR_KAYE_CWD", "HERDR_ACTIVE_PANE_CWD"] {
        if let Ok(val) = env::var(key)
            && !val.is_empty() {
                return PathBuf::from(val);
            }
    }
    if let Ok(ctx) = env::var("HERDR_PLUGIN_CONTEXT_JSON")
        && let Some(cwd) = extract_cwd_from_json(&ctx) {
            return PathBuf::from(cwd);
        }
    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn resolve_active_pane_cwd(herdr_bin: &str) -> PathBuf {
    let plugin_root = env::var("HERDR_PLUGIN_ROOT").ok();
    let ctx = env::var("HERDR_PLUGIN_CONTEXT_JSON").ok();

    if let Some(ref c) = ctx {
        // If focused pane is a KAYE pane (running from plugin root), prefer a sibling terminal pane in the tab.
        let focused_is_plugin = extract_json_string(c, "focused_pane_cwd")
            .zip(plugin_root.as_deref())
            .is_some_and(|(fc, pr)| fc == pr);
        if !focused_is_plugin
            && let Some(cwd) = extract_cwd_from_json(c)
                && plugin_root.as_deref() != Some(cwd.as_str()) {
                    return PathBuf::from(cwd);
                }
    }

    // Look up a non-KAYE pane in the active tab.
    if let Some(tab_id) = env::var("HERDR_TAB_ID").ok().filter(|s| !s.is_empty()) {
        let mut cmd = Command::new(herdr_bin);
        cmd.args(["pane", "list"]);
        if let Ok(ws) = env::var("HERDR_WORKSPACE_ID")
            && !ws.is_empty() {
                cmd.args(["--workspace", &ws]);
            }
        if let Ok(out) = cmd.output()
            && out.status.success() {
                let body = String::from_utf8_lossy(&out.stdout);
                if let Some(cwd) = find_non_kaye_pane_cwd(&body, &tab_id, plugin_root.as_deref()) {
                    return PathBuf::from(cwd);
                }
            }
    }

    if let Some(ref c) = ctx
        && let Some(ws_cwd) = extract_json_string(c, "workspace_cwd").filter(|s| !s.is_empty())
            && plugin_root.as_deref() != Some(ws_cwd.as_str()) {
                return PathBuf::from(ws_cwd);
            }

    if let Ok(pane_id) = env::var("HERDR_PANE_ID")
        && !pane_id.is_empty()
            && let Ok(out) = Command::new(herdr_bin).args(["pane", "get", &pane_id]).output()
                && out.status.success() {
                    let body = String::from_utf8_lossy(&out.stdout);
                    if let Some(cwd) = extract_cwd_from_json(&body)
                        && plugin_root.as_deref() != Some(cwd.as_str()) {
                            return PathBuf::from(cwd);
                        }
                }
    if let Ok(out) = Command::new(herdr_bin).args(["pane", "current"]).output()
        && out.status.success() {
            let body = String::from_utf8_lossy(&out.stdout);
            if let Some(cwd) = extract_cwd_from_json(&body)
                && plugin_root.as_deref() != Some(cwd.as_str()) {
                    return PathBuf::from(cwd);
                }
        }
    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

// Prefer live pane cwd from Herdr plugin context or `pane get` output.
fn extract_cwd_from_json(json: &str) -> Option<String> {
    extract_json_string(json, "focused_pane_cwd")
        .or_else(|| extract_json_string(json, "foreground_cwd"))
        .or_else(|| extract_json_string(json, "workspace_cwd"))
        .or_else(|| extract_json_string(json, "cwd"))
        .filter(|s| !s.is_empty())
}

fn find_existing_kaye_pane(herdr_bin: &str) -> Option<String> {
    let tab_id = env::var("HERDR_TAB_ID").ok().filter(|s| !s.is_empty())?;
    let mut cmd = Command::new(herdr_bin);
    cmd.args(["pane", "list"]);
    if let Ok(ws) = env::var("HERDR_WORKSPACE_ID")
        && !ws.is_empty() {
            cmd.args(["--workspace", &ws]);
        }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let body = String::from_utf8_lossy(&out.stdout);
    find_pane_id_by_label_and_tab(&body, "KAYE", &tab_id)
}

fn find_pane_id_by_label_and_tab(json: &str, label: &str, tab_id: &str) -> Option<String> {
    let needle = format!("\"label\":\"{label}\"");
    let mut offset = 0;
    while let Some(rel_pos) = json[offset..].find(&needle) {
        let abs_pos = offset + rel_pos;
        if let Some((start, end)) = enclosing_json_object(json, abs_pos) {
            let obj = &json[start..end];
            if extract_json_string(obj, "tab_id").as_deref() == Some(tab_id)
                && let Some(pid) = extract_json_string(obj, "pane_id") {
                    return Some(pid);
                }
            offset = end;
        } else {
            break;
        }
    }
    None
}

fn find_non_kaye_pane_cwd(json: &str, tab_id: &str, plugin_root: Option<&str>) -> Option<String> {
    let needle = format!("\"tab_id\":\"{tab_id}\"");
    let mut offset = 0;
    while let Some(rel_pos) = json[offset..].find(&needle) {
        let abs_pos = offset + rel_pos;
        if let Some((start, end)) = enclosing_json_object(json, abs_pos) {
            let obj = &json[start..end];
            let is_kaye = extract_json_string(obj, "label").as_deref() == Some("KAYE");
            if !is_kaye
                && let Some(cwd) = extract_json_string(obj, "foreground_cwd")
                    .or_else(|| extract_json_string(obj, "cwd"))
                    .filter(|s| !s.is_empty())
                    && plugin_root != Some(cwd.as_str()) {
                        return Some(cwd);
                    }
            offset = end;
        } else {
            break;
        }
    }
    None
}

fn enclosing_json_object(json: &str, pos: usize) -> Option<(usize, usize)> {
    let mut back_depth = 0usize;
    let mut obj_start = None;
    for (i, ch) in json[..pos].char_indices().rev() {
        match ch {
            '}' => back_depth += 1,
            '{' => {
                if back_depth == 0 {
                    obj_start = Some(i);
                    break;
                }
                back_depth -= 1;
            }
            _ => {}
        }
    }
    let start = obj_start?;
    let mut depth = 0usize;
    for (i, ch) in json[start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some((start, start + i + 1));
                }
            }
            _ => {}
        }
    }
    None
}

fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let mut search = json;
    while let Some(pos) = search.find(&needle) {
        let after_key = &search[pos + needle.len()..];
        let trimmed = after_key.trim_start();
        if let Some(after_colon) = trimmed.strip_prefix(':') {
            let val_start = after_colon.trim_start();
            if let Some(inner) = val_start.strip_prefix('"') {
                return parse_json_str(inner);
            }
        }
        search = &search[pos + needle.len()..];
    }
    None
}

fn parse_json_str(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                other => out.push(other),
            },
            other => out.push(other),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kaye::{
        CheckpointRange, FileChange, FileStatus, LineKind, aggregate_dirs, build_full_code_view,
        parse_what_change,
    };
    use std::collections::BTreeMap;

    #[test]
    fn json_cwd_prefers_focused_and_foreground_cwd() {
        let ctx = r#"{"workspace_cwd":"/home/user/root","focused_pane_cwd":"/home/user/root/app"}"#;
        assert_eq!(extract_cwd_from_json(ctx).as_deref(), Some("/home/user/root/app"));

        let sample = r#"{"pane":{"cwd":"/home/user/root","foreground_cwd":"/home/user/root/src"}}"#;
        assert_eq!(
            extract_cwd_from_json(sample).as_deref(),
            Some("/home/user/root/src")
        );
    }

    #[test]
    fn json_cwd_handles_escapes_and_fallback() {
        let sample = r#"{"cwd":"/tmp/a\"b\\c"}"#;
        assert_eq!(extract_cwd_from_json(sample).as_deref(), Some(r#"/tmp/a"b\c"#));
    }

    #[test]
    fn what_change_parses_headers_counts_and_hunks() {
        let sample = "\
================================================================
[df91b4f] update cwd (1 minute ago)
================================================================

  [MODIFIED] src/main.rs
    @@ -2,3 +2,4 @@ fn demo() {
     keep1
    -old_line
    +new_line_1
    +new_line_2
     keep2

  [NEW] src/new.rs
    @@ -0,0 +1,2 @@
    +fn a() {}
    +fn b() {}
";
        let (msg, files) = parse_what_change(sample);
        assert_eq!(msg, "update cwd");
        let main = &files["src/main.rs"];
        assert_eq!(main.status, FileStatus::Modified);
        assert_eq!((main.added, main.deleted), (2, 1));
        assert_eq!(main.hunks.len(), 1);
        assert_eq!(main.hunks[0].new_start, 2);

        let new_f = &files["src/new.rs"];
        assert_eq!(new_f.status, FileStatus::Added);
        assert_eq!((new_f.added, new_f.deleted), (2, 0));
    }

    #[test]
    fn full_code_view_overlays_inline_deletions_and_additions() {
        let sample = concat!(
            "  [MODIFIED] f.rs\n",
            "    @@ -2,3 +2,4 @@\n",
            "     line2\n",
            "    -old3\n",
            "    +new3a\n",
            "    +new3b\n",
            "     line4\n",
        );
        let (_, files) = parse_what_change(sample);
        let disk: Vec<String> = ["line1", "line2", "new3a", "new3b", "line4", "line5"]
            .into_iter()
            .map(String::from)
            .collect();
        let view = build_full_code_view(&disk, &files["f.rs"].hunks);
        assert_eq!(view.len(), 7);
        assert_eq!(view[0].kind, LineKind::Context);
        assert_eq!(view[1].kind, LineKind::Context);
        assert_eq!(view[2].kind, LineKind::Deleted);
        assert_eq!(view[2].text, "old3");
        assert_eq!(view[3].kind, LineKind::Added);
        assert_eq!(view[3].line_no, Some(3));
        assert_eq!(view[4].kind, LineKind::Added);
        assert_eq!(view[4].line_no, Some(4));
        assert_eq!(view[5].kind, LineKind::Context);
        assert_eq!(view[6].kind, LineKind::Context);
    }

    #[test]
    fn dir_aggregation_bubbles_stats_to_ancestors() {
        let mut changes = BTreeMap::new();
        changes.insert(
            "src/ui/panel.rs".into(),
            FileChange {
                status: FileStatus::Modified,
                added: 10,
                deleted: 3,
                hunks: Vec::new(),
            },
        );
        changes.insert(
            "src/kaye.rs".into(),
            FileChange {
                status: FileStatus::Added,
                added: 25,
                deleted: 0,
                hunks: Vec::new(),
            },
        );
        let dirs = aggregate_dirs(&changes);
        assert_eq!(dirs[""].changed_files, 2);
        assert_eq!(dirs[""].added, 35);
        assert_eq!(dirs[""].deleted, 3);
        assert_eq!(dirs["src"].changed_files, 2);
        assert_eq!(dirs["src/ui"].added, 10);
        assert_eq!(dirs["src/ui"].deleted, 3);
    }

    #[test]
    fn existing_kaye_pane_matches_label_and_tab_with_nested_objects() {
        let sample = r#"{"result":{"panes":[{"meta":{"nested":1},"label":"KAYE","pane_id":"wE:p2","tab_id":"wE:t1"},{"nested":{"x":2},"label":"KAYE","pane_id":"wD:p2","scroll":{"viewport_rows":57},"tab_id":"wD:t1"}]}}"#;
        assert_eq!(
            find_pane_id_by_label_and_tab(sample, "KAYE", "wD:t1").as_deref(),
            Some("wD:p2")
        );
        assert_eq!(
            find_pane_id_by_label_and_tab(sample, "KAYE", "wD:t2"),
            None
        );
    }

    #[test]
    fn sgr_mouse_parses_click_and_wheel() {
        assert_eq!(
            tui::parse_sgr_mouse("0;12;5M"),
            tui::Key::MouseClick { col: 12, row: 5 }
        );
        assert_eq!(
            tui::parse_sgr_mouse("64;40;10M"),
            tui::Key::MouseScrollUp { col: 40 }
        );
        assert_eq!(
            tui::parse_sgr_mouse("65;40;10M"),
            tui::Key::MouseScrollDown { col: 40 }
        );
        assert_eq!(tui::parse_sgr_mouse("0;12;5m"), tui::Key::Other);
    }

    #[test]
    fn checkpoint_range_maps_initial_1_to_latest_11() {
        assert_eq!(CheckpointRange::Latest.to_what_change_arg(11), "1");
        assert_eq!(CheckpointRange::From(11).to_what_change_arg(11), "1");
        assert_eq!(CheckpointRange::From(9).to_what_change_arg(11), "3");
        assert_eq!(CheckpointRange::From(1).to_what_change_arg(11), "11");
        assert_eq!(CheckpointRange::All.to_what_change_arg(11), "all");
    }

    #[test]
    fn non_kaye_pane_cwd_skips_kaye_pane_and_plugin_root() {
        let plugin_root = "/home/ikaye/.config/herdr/plugins/github-installations/ikaye.kaye/checkout";
        let sample = format!(
            r#"{{"result":{{"panes":[{{"cwd":"{plugin_root}","foreground_cwd":"{plugin_root}","label":"KAYE","pane_id":"wD:pQ","tab_id":"wD:t1"}},{{"cwd":"/home/ikaye/Projects/my-app","foreground_cwd":"/home/ikaye/Projects/my-app","pane_id":"wD:p1","tab_id":"wD:t1"}}]}}}}"#
        );
        assert_eq!(
            find_non_kaye_pane_cwd(&sample, "wD:t1", Some(plugin_root)).as_deref(),
            Some("/home/ikaye/Projects/my-app")
        );
    }
}
