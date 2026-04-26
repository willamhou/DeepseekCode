use crate::model::protocol::{Observation, ObservationKind};

const FILE_EXCERPT_LINES: usize = 60;
const LISTING_LINES: usize = 40;
const SEARCH_RESULT_LINES: usize = 40;
const PATCH_LINES: usize = 40;
const DIFF_LINES: usize = 80;
const SHELL_TAIL_LINES: usize = 60;
const OTHER_LINES: usize = 40;
const SUPERSEDED_PREFIX: &str = "(superseded by a later observation of the same kind; full content dropped to free context budget)";

pub fn summarize_for_kind(raw: &str, kind: ObservationKind) -> String {
    match kind {
        ObservationKind::ShellOutput => tail_trim(raw, SHELL_TAIL_LINES),
        ObservationKind::FileExcerpt => head_trim(raw, FILE_EXCERPT_LINES),
        ObservationKind::Listing => head_trim(raw, LISTING_LINES),
        ObservationKind::SearchResults => head_trim(raw, SEARCH_RESULT_LINES),
        ObservationKind::Patch => head_trim(raw, PATCH_LINES),
        ObservationKind::Diff => trim_diff(raw, DIFF_LINES),
        ObservationKind::Other => head_trim(raw, OTHER_LINES),
    }
}

pub fn compact_observations(observations: &[Observation]) -> Vec<Observation> {
    let mut latest_index_for_kind: std::collections::HashMap<ObservationKind, usize> =
        std::collections::HashMap::new();

    for (index, observation) in observations.iter().enumerate() {
        if observation.is_failure() {
            continue;
        }
        latest_index_for_kind.insert(observation.kind, index);
    }

    observations
        .iter()
        .enumerate()
        .map(|(index, observation)| {
            if observation.is_failure() {
                return observation.clone();
            }
            let latest = latest_index_for_kind.get(&observation.kind).copied();
            if latest != Some(index) {
                let mut stub = observation.clone();
                stub.summary = format!(
                    "{SUPERSEDED_PREFIX} (kind={})",
                    observation.kind.label()
                );
                return stub;
            }
            observation.clone()
        })
        .collect()
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

fn tail_trim(raw: &str, max_lines: usize) -> String {
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

    let mut keep = vec![false; lines.len()];
    for &index in &header_indices {
        keep[index] = true;
        for offset in 1..=2 {
            if index + offset < lines.len() {
                keep[index + offset] = true;
            }
        }
    }

    let mut output = String::new();
    let mut dropped = 0usize;
    let mut last_kept = false;
    for (index, line) in lines.iter().enumerate() {
        if keep[index] {
            if !last_kept && (dropped > 0 || index > 0) {
                if !output.is_empty() {
                    output.push('\n');
                }
                if dropped > 0 {
                    output.push_str(&format!("... dropped {dropped} body lines ...\n"));
                    dropped = 0;
                }
            } else if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(line);
            last_kept = true;
        } else {
            dropped += 1;
            last_kept = false;
        }
    }
    if dropped > 0 {
        output.push_str(&format!("\n... dropped {dropped} trailing body lines ..."));
    }
    output
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
}
