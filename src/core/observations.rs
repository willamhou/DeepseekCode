use crate::model::protocol::{Observation, ObservationKind};

const FILE_EXCERPT_LINES: usize = 60;
const LISTING_LINES: usize = 40;
const SEARCH_RESULT_LINES: usize = 40;
const PATCH_LINES: usize = 40;
const DIFF_LINES: usize = 80;
const SHELL_TAIL_LINES: usize = 60;
const OTHER_LINES: usize = 40;

pub fn summarize_for_kind(raw: &str, kind: ObservationKind) -> String {
    match kind {
        ObservationKind::ShellOutput => tail_trim(raw, SHELL_TAIL_LINES),
        ObservationKind::FileExcerpt => head_trim(raw, FILE_EXCERPT_LINES),
        ObservationKind::Listing => head_trim(raw, LISTING_LINES),
        ObservationKind::SearchResults => head_trim(raw, SEARCH_RESULT_LINES),
        ObservationKind::Patch => head_trim(raw, PATCH_LINES),
        ObservationKind::Diff => trim_diff(raw, DIFF_LINES),
        ObservationKind::Other => head_trim(raw, OTHER_LINES),
        ObservationKind::Todos => raw.lines().next().unwrap_or(raw).to_string(),
    }
}

pub fn compact_observations(observations: &[Observation]) -> Vec<Observation> {
    let mut latest_for_kind: [Option<usize>; KIND_COUNT] = [None; KIND_COUNT];
    for (index, observation) in observations.iter().enumerate() {
        if observation.is_failure() {
            continue;
        }
        latest_for_kind[kind_index(observation.kind)] = Some(index);
    }

    observations
        .iter()
        .enumerate()
        .map(|(index, observation)| {
            if observation.is_failure() {
                return observation.clone();
            }
            if latest_for_kind[kind_index(observation.kind)] != Some(index) {
                let mut stub = observation.clone();
                stub.summary = supersede_stub(&observation.summary, observation.kind);
                return stub;
            }
            observation.clone()
        })
        .collect()
}

fn supersede_stub(summary: &str, kind: ObservationKind) -> String {
    let total_lines = summary.lines().count();
    let first_line = summary
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let preview = if first_line.chars().count() > 80 {
        let head: String = first_line.chars().take(80).collect();
        format!("{head}…")
    } else {
        first_line.to_string()
    };
    if preview.is_empty() {
        format!("(superseded; kind={}, was {total_lines} line(s))", kind.label())
    } else {
        format!(
            "(superseded; kind={}, was {total_lines} line(s), first: {preview:?})",
            kind.label()
        )
    }
}

const KIND_COUNT: usize = 8;

fn kind_index(kind: ObservationKind) -> usize {
    let index = match kind {
        ObservationKind::FileExcerpt => 0,
        ObservationKind::Listing => 1,
        ObservationKind::SearchResults => 2,
        ObservationKind::Patch => 3,
        ObservationKind::Diff => 4,
        ObservationKind::ShellOutput => 5,
        ObservationKind::Other => 6,
        ObservationKind::Todos => 7,
    };
    debug_assert!(index < KIND_COUNT);
    index
}

fn head_trim(raw: &str, max_lines: usize) -> String {
    let total = raw.lines().count();
    if total <= max_lines {
        return raw.trim_end_matches('\n').to_string();
    }
    let mut output = raw
        .lines()
        .take(max_lines)
        .collect::<Vec<_>>()
        .join("\n");
    output.push_str(&format!("\n... truncated {} more lines ...", total - max_lines));
    output
}

pub fn tail_trim(raw: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = raw.lines().collect();
    if lines.len() <= max_lines {
        return raw.trim_end_matches('\n').to_string();
    }
    let dropped = lines.len() - max_lines;
    let tail = lines[dropped..].join("\n");
    format!("... truncated {dropped} earlier lines ...\n{tail}")
}

fn trim_diff(raw: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = raw.lines().collect();
    if lines.len() <= max_lines {
        return raw.trim_end_matches('\n').to_string();
    }

    let header_indices: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            if line.starts_with("@@") || line.starts_with("diff --git") || line.starts_with("--- ") || line.starts_with("+++ ") {
                Some(index)
            } else {
                None
            }
        })
        .collect();

    if header_indices.is_empty() {
        return head_trim(raw, max_lines);
    }

    let header_budget = max_lines.saturating_sub(2).max(1);
    let mut keep = vec![false; lines.len()];
    let mut kept_count = 0usize;
    'outer: for &index in &header_indices {
        for offset in 0..=2 {
            let target = index + offset;
            if target >= lines.len() || keep[target] {
                continue;
            }
            keep[target] = true;
            kept_count += 1;
            if kept_count >= header_budget {
                break 'outer;
            }
        }
    }

    let last_kept_index = keep.iter().rposition(|&kept| kept).unwrap_or(0);

    let mut out_lines: Vec<String> = Vec::with_capacity(max_lines);
    let mut dropped = 0usize;
    let mut last_kept = false;
    for (index, line) in lines.iter().enumerate().take(last_kept_index + 1) {
        if out_lines.len() + 1 >= max_lines {
            break;
        }
        if keep[index] {
            if !last_kept && dropped > 0 {
                out_lines.push(format!("... dropped {dropped} body lines ..."));
                dropped = 0;
            }
            out_lines.push((*line).to_string());
            last_kept = true;
        } else {
            dropped += 1;
            last_kept = false;
        }
    }

    let unrendered = lines.len() - (last_kept_index + 1);
    let truncated_in_loop = out_lines.len() + 1 >= max_lines;
    if dropped > 0 {
        out_lines.push(format!("... dropped {dropped} trailing lines ..."));
    } else if unrendered > 0 || truncated_in_loop {
        let total_remaining = unrendered + dropped;
        out_lines.push(format!("... {total_remaining} more lines elided ..."));
    }

    out_lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::protocol::ObservationStatus;

    #[test]
    fn shell_output_trims_to_tail() {
        let raw = (1..=200)
            .map(|n| format!("line{n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let trimmed = summarize_for_kind(&raw, ObservationKind::ShellOutput);
        assert!(trimmed.starts_with("... truncated"));
        assert!(trimmed.contains("line200"));
        assert!(!trimmed.contains("line1\n"));
    }

    #[test]
    fn file_excerpt_trims_to_head() {
        let raw = (1..=200)
            .map(|n| format!("line{n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let trimmed = summarize_for_kind(&raw, ObservationKind::FileExcerpt);
        assert!(trimmed.starts_with("line1"));
        assert!(trimmed.contains("... truncated"));
        assert!(!trimmed.contains("line200"));
    }

    #[test]
    fn search_results_trim_to_head() {
        let raw = (1..=100)
            .map(|n| format!("match{n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let trimmed = summarize_for_kind(&raw, ObservationKind::SearchResults);
        assert!(trimmed.contains("match1"));
        assert!(!trimmed.contains("match100"));
    }

    #[test]
    fn diff_trim_honours_max_lines_with_many_hunks() {
        let mut raw = String::new();
        for hunk in 0..200 {
            raw.push_str(&format!(
                "@@ -{0},1 +{0},1 @@\nbody-{0}-1\nbody-{0}-2\n",
                hunk
            ));
        }
        let trimmed = trim_diff(&raw, 80);
        let total_lines = trimmed.lines().count();
        assert!(total_lines <= 80, "got {total_lines} lines, expected <= 80");
        assert!(trimmed.contains("@@ -0,1 +0,1 @@"));
    }

    #[test]
    fn diff_preserves_hunk_headers() {
        let raw = format!(
            "diff --git a/foo b/foo\n--- a/foo\n+++ b/foo\n@@ -1,200 +1,200 @@\n{}\n@@ -300,5 +300,5 @@\n{}",
            (1..=100).map(|n| format!(" body{n}")).collect::<Vec<_>>().join("\n"),
            (1..=100).map(|n| format!(" tail{n}")).collect::<Vec<_>>().join("\n"),
        );
        let trimmed = summarize_for_kind(&raw, ObservationKind::Diff);
        assert!(trimmed.contains("@@ -1,200 +1,200 @@"));
        assert!(trimmed.contains("@@ -300,5 +300,5 @@"));
        assert!(trimmed.contains("dropped"));
    }

    #[test]
    fn small_outputs_pass_through_unchanged() {
        let raw = "one line";
        assert_eq!(
            summarize_for_kind(raw, ObservationKind::ShellOutput),
            "one line"
        );
        assert_eq!(
            summarize_for_kind(raw, ObservationKind::FileExcerpt),
            "one line"
        );
    }

    #[test]
    fn compact_observations_keeps_only_latest_per_kind() {
        let observations = vec![
            Observation::ok("read_file", "first read content"),
            Observation::ok("list_files", "directory listing"),
            Observation::ok("read_file", "second read content"),
        ];

        let compacted = compact_observations(&observations);
        assert_eq!(compacted.len(), 3);
        assert!(compacted[0].summary.starts_with("(superseded"));
        assert!(compacted[0].summary.contains("file_excerpt"));
        assert_eq!(compacted[1].summary, "directory listing");
        assert_eq!(compacted[2].summary, "second read content");
    }

    #[test]
    fn supersede_stub_includes_first_line_and_count() {
        let original = "fn main() {\n    println!(\"hello\");\n}\n";
        let observations = vec![
            Observation::ok("read_file", original),
            Observation::ok("read_file", "second"),
        ];
        let compacted = compact_observations(&observations);
        let stub = &compacted[0].summary;
        assert!(stub.contains("file_excerpt"));
        assert!(stub.contains("3 line(s)"));
        assert!(stub.contains("fn main()"));
    }

    #[test]
    fn supersede_stub_truncates_long_first_line() {
        let long_line = "x".repeat(200);
        let observations = vec![
            Observation::ok("read_file", long_line.clone()),
            Observation::ok("read_file", "second"),
        ];
        let compacted = compact_observations(&observations);
        let stub = &compacted[0].summary;
        assert!(stub.contains("…"));
        assert!(!stub.contains(&"x".repeat(120)));
    }

    #[test]
    fn compact_observations_preserves_failures_verbatim() {
        let observations = vec![
            Observation::failed("apply_patch", "first failure"),
            Observation::ok("read_file", "successful read"),
            Observation::failed("apply_patch", "second failure"),
        ];

        let compacted = compact_observations(&observations);
        assert_eq!(compacted.len(), 3);
        assert!(matches!(compacted[0].status, ObservationStatus::Failed));
        assert_eq!(compacted[0].summary, "first failure");
        assert_eq!(compacted[1].summary, "successful read");
        assert_eq!(compacted[2].summary, "second failure");
    }

    #[test]
    fn compact_observations_does_not_supersede_other_kind_failures() {
        let observations = vec![
            Observation::ok("read_file", "first read"),
            Observation::failed("read_file", "read error"),
            Observation::ok("read_file", "second read"),
        ];

        let compacted = compact_observations(&observations);
        assert!(compacted[0].summary.starts_with("(superseded"));
        assert_eq!(compacted[1].summary, "read error");
        assert_eq!(compacted[2].summary, "second read");
    }

    #[test]
    fn from_tool_name_maps_todo_write_to_todos() {
        assert_eq!(ObservationKind::from_tool_name("todo_write"), ObservationKind::Todos);
    }

    #[test]
    fn label_for_todos_is_todos() {
        assert_eq!(ObservationKind::Todos.label(), "todos");
    }

    #[test]
    fn kind_index_for_todos_is_seven_within_kind_count() {
        let idx = kind_index(ObservationKind::Todos);
        assert_eq!(idx, 7);
        assert!(idx < KIND_COUNT);
    }

    #[test]
    fn compact_observations_supersedes_old_todos_observation() {
        let observations = vec![
            Observation::ok("todo_write", "5 todos: 0 completed, 1 in_progress, 4 pending\n  details..."),
            Observation::ok("read_file", "some file"),
            Observation::ok("todo_write", "5 todos: 1 completed, 1 in_progress, 3 pending\n  newer..."),
        ];
        let compacted = compact_observations(&observations);
        assert!(
            compacted[0].summary.starts_with("(superseded"),
            "old todos should be superseded: {}",
            compacted[0].summary
        );
        assert!(compacted[2].summary.starts_with("5 todos: 1 completed"));
    }
}
