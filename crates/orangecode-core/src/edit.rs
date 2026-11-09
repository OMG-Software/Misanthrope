//! Bounded, non-destructive edit previews. Replacement is not implemented.
//!
//! A preview compares exact current and proposed UTF-8 content and returns a
//! single surrounding diff hunk. It never writes to the workspace, creates a
//! checkpoint, or grants replacement authority.

use std::io::Read;
use std::path::Path;

use orangecode_platform::open_regular_file_readonly;
use serde::Serialize;
use thiserror::Error;

use crate::session::NativePath;

/// Maximum accepted size of either side of an edit preview.
pub const MAX_EDIT_CONTENT_BYTES: usize = 1024 * 1024;
/// Maximum line count considered for either side of a changed diff.
pub const MAX_EDIT_LINES: usize = 65_536;
/// Maximum rendered diff text returned by an edit preview.
pub const MAX_DIFF_BYTES: usize = 64 * 1024;
/// Context lines retained before and after the changed region.
const CONTEXT_LINES: usize = 3;
const DIFF_HEADER: &str = "--- current\n+++ proposed\n";
const TRUNCATION_MARKER: &str = "... diff truncated ...\n";
const NO_NEWLINE_MARKER: &str = "\\ No newline at end of file\n";

/// One read-only edit preview. It is evidence, not a write grant.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EditPreview {
    /// Lossless workspace-relative target metadata.
    pub target: NativePath,
    /// Whether the exact bytes differ.
    pub changed: bool,
    /// SHA-256 of the current bytes read through the broker.
    pub current_sha256: String,
    /// Exact current byte count.
    pub current_bytes: u64,
    /// SHA-256 of the trusted proposed content.
    pub proposed_sha256: String,
    /// Exact proposed byte count.
    pub proposed_bytes: u64,
    /// Bounded single-hunk diff text, or empty when unchanged.
    pub diff: String,
    /// Whether the diff text reached its bound.
    pub truncated: bool,
}

/// A controlled edit-preview failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum EditError {
    /// The current content exceeds the edit-preview limit.
    #[error("current file exceeds the edit-preview limit")]
    CurrentTooLarge,
    /// The proposed content exceeds the edit-preview limit.
    #[error("proposed content exceeds the edit-preview limit")]
    ProposedTooLarge,
    /// The current content is not UTF-8 text.
    #[error("current file is not valid UTF-8 text")]
    CurrentNotText,
    /// The proposed content is not UTF-8 text.
    #[error("proposed content is not valid UTF-8 text")]
    ProposedNotText,
    /// The changed content has too many lines for a bounded preview.
    #[error("edit preview exceeds the line-count limit")]
    TooManyLines,
}

/// A controlled failure while reading trusted proposed content.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ProposedReadError {
    /// The proposed source exceeds the selected limit.
    #[error("proposed content exceeds the edit-preview limit")]
    TooLarge,
    /// The proposed source changed size during the read.
    #[error("proposed content changed while being read")]
    ChangedDuringRead,
    /// The proposed source is a link, reparse point, device, or non-regular file.
    #[error("proposed content is not a regular non-link file")]
    NotRegular,
    /// Trusted regular-file opening is unavailable on this platform.
    #[error("trusted proposed-content opening is unavailable on this platform")]
    Unsupported,
    /// An operating-system read or metadata operation failed.
    #[error("proposed content could not be read")]
    Io,
}

/// Open and read complete trusted proposed content with a bounded size.
///
/// The source is opened read-only without following a final symlink or Windows
/// reparse point. It must remain a regular file and retain its initial length
/// for the whole read. This is trusted operator input, not workspace authority.
pub fn read_proposed_file(path: &Path, limit: usize) -> Result<Vec<u8>, ProposedReadError> {
    if limit == 0 {
        return Err(ProposedReadError::TooLarge);
    }
    let mut file = open_regular_file_readonly(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::InvalidInput => ProposedReadError::NotRegular,
        std::io::ErrorKind::Unsupported => ProposedReadError::Unsupported,
        _ => ProposedReadError::Io,
    })?;
    let initial_len = file.metadata().map_err(|_| ProposedReadError::Io)?.len();
    let limit = u64::try_from(limit).map_err(|_| ProposedReadError::TooLarge)?;
    if initial_len > limit {
        return Err(ProposedReadError::TooLarge);
    }
    let read_limit = limit.checked_add(1).ok_or(ProposedReadError::TooLarge)?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|_| ProposedReadError::Io)?;
    if u64::try_from(bytes.len()).map_err(|_| ProposedReadError::TooLarge)? > limit {
        return Err(ProposedReadError::TooLarge);
    }
    if u64::try_from(bytes.len()).map_err(|_| ProposedReadError::TooLarge)? != initial_len
        || file.metadata().map_err(|_| ProposedReadError::Io)?.len() != initial_len
    {
        return Err(ProposedReadError::ChangedDuringRead);
    }
    Ok(bytes)
}

/// Build one bounded, non-destructive preview from exact current and proposed bytes.
///
/// The diff is a single hunk around the common prefix and suffix, not a minimal
/// line diff. It is truncated at `MAX_DIFF_BYTES`; the digests and sizes always
/// describe the complete inputs. This function performs no I/O and grants no
/// permission.
pub fn preview(
    current: &[u8],
    proposed: &[u8],
    target: NativePath,
) -> Result<EditPreview, EditError> {
    if current.len() > MAX_EDIT_CONTENT_BYTES {
        return Err(EditError::CurrentTooLarge);
    }
    if proposed.len() > MAX_EDIT_CONTENT_BYTES {
        return Err(EditError::ProposedTooLarge);
    }
    let current_text = std::str::from_utf8(current).map_err(|_| EditError::CurrentNotText)?;
    let proposed_text = std::str::from_utf8(proposed).map_err(|_| EditError::ProposedNotText)?;
    let current_bytes = u64::try_from(current.len()).map_err(|_| EditError::CurrentTooLarge)?;
    let proposed_bytes = u64::try_from(proposed.len()).map_err(|_| EditError::ProposedTooLarge)?;
    let current_sha256 = crate::session::sha256_hex(current);
    let proposed_sha256 = crate::session::sha256_hex(proposed);
    let changed = current_sha256 != proposed_sha256;
    let (diff, truncated) = if changed {
        if current_text.split_inclusive('\n').count() > MAX_EDIT_LINES
            || proposed_text.split_inclusive('\n').count() > MAX_EDIT_LINES
        {
            return Err(EditError::TooManyLines);
        }
        diff(current_text, proposed_text)
    } else {
        (String::new(), false)
    };
    Ok(EditPreview {
        target,
        changed,
        current_sha256,
        current_bytes,
        proposed_sha256,
        proposed_bytes,
        diff,
        truncated,
    })
}

/// Render one bounded hunk around the common prefix and suffix.
fn diff(current: &str, proposed: &str) -> (String, bool) {
    let current_lines: Vec<&str> = current.split_inclusive('\n').collect();
    let proposed_lines: Vec<&str> = proposed.split_inclusive('\n').collect();

    let mut prefix = 0;
    while prefix < current_lines.len()
        && prefix < proposed_lines.len()
        && current_lines[prefix] == proposed_lines[prefix]
    {
        prefix = prefix.saturating_add(1);
    }
    let max_suffix = current_lines
        .len()
        .min(proposed_lines.len())
        .saturating_sub(prefix);
    let mut suffix = 0;
    while suffix < max_suffix
        && current_lines[current_lines.len() - 1 - suffix]
            == proposed_lines[proposed_lines.len() - 1 - suffix]
    {
        suffix = suffix.saturating_add(1);
    }

    let current_middle = &current_lines[prefix..current_lines.len() - suffix];
    let proposed_middle = &proposed_lines[prefix..proposed_lines.len() - suffix];
    let context_start = prefix.saturating_sub(CONTEXT_LINES);
    let context_end = current_lines
        .len()
        .saturating_sub(suffix)
        .saturating_add(CONTEXT_LINES)
        .min(current_lines.len());

    let mut output = String::from(DIFF_HEADER);
    let mut truncated = false;
    for line in &current_lines[context_start..prefix] {
        if !append_line(&mut output, line, ' ') {
            truncated = true;
            break;
        }
    }
    if !truncated {
        for line in current_middle {
            if !append_line(&mut output, line, '-') {
                truncated = true;
                break;
            }
        }
    }
    if !truncated {
        for line in proposed_middle {
            if !append_line(&mut output, line, '+') {
                truncated = true;
                break;
            }
        }
    }
    if !truncated {
        for line in &current_lines[current_lines.len() - suffix..context_end] {
            if !append_line(&mut output, line, ' ') {
                truncated = true;
                break;
            }
        }
    }
    if truncated {
        output.push_str(TRUNCATION_MARKER);
    }
    (output, truncated)
}

/// Append one prefixed line while reserving room for the truncation marker.
fn append_line(output: &mut String, line: &str, marker: char) -> bool {
    let no_newline = !line.ends_with('\n');
    let needed = line.len().saturating_add(2).saturating_add(if no_newline {
        NO_NEWLINE_MARKER.len()
    } else {
        0
    });
    if output
        .len()
        .saturating_add(needed)
        .saturating_add(TRUNCATION_MARKER.len())
        > MAX_DIFF_BYTES
    {
        return false;
    }
    output.push(marker);
    output.push_str(line);
    if no_newline {
        output.push('\n');
        output.push_str(NO_NEWLINE_MARKER);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::NativePath;

    fn target() -> NativePath {
        NativePath::UnixBytes(b"src/main.rs".to_vec())
    }

    #[test]
    fn previews_one_localized_hunk_with_surrounding_context() {
        let preview = preview(b"one\ntwo\nthree\n", b"one\nTWO\nthree\n", target())
            .expect("valid UTF-8 preview");

        assert!(preview.changed);
        assert!(!preview.truncated);
        assert_eq!(preview.current_bytes, 14);
        assert_eq!(preview.proposed_bytes, 14);
        assert_eq!(preview.current_sha256.len(), 64);
        assert_eq!(
            preview.diff,
            "--- current\n+++ proposed\n one\n-two\n+TWO\n three\n"
        );
    }

    #[test]
    fn unchanged_content_returns_no_diff() {
        let preview = preview(b"same\n", b"same\n", target()).expect("valid preview");

        assert!(!preview.changed);
        assert_eq!(preview.diff, "");
        assert!(!preview.truncated);
        assert_eq!(preview.current_sha256, preview.proposed_sha256);
    }

    #[test]
    fn insertions_deletions_and_missing_final_newlines_are_visible() {
        let inserted = preview(b"a\n", b"a\nb\n", target()).expect("insertion");
        assert_eq!(inserted.diff, "--- current\n+++ proposed\n a\n+b\n");

        let deleted = preview(b"a\nb\n", b"a\n", target()).expect("deletion");
        assert_eq!(deleted.diff, "--- current\n+++ proposed\n a\n-b\n");

        let no_final_newline = preview(b"a\nb", b"a\nB", target()).expect("no final newline");
        assert_eq!(
            no_final_newline.diff,
            "--- current\n+++ proposed\n a\n-b\n\\ No newline at end of file\n+B\n\\ No newline at end of file\n"
        );
    }

    #[test]
    fn a_final_newline_only_change_is_visible() {
        let preview = preview(b"a", b"a\n", target()).expect("final newline change");

        assert!(preview.changed);
        assert_eq!(
            preview.diff,
            "--- current\n+++ proposed\n-a\n\\ No newline at end of file\n+a\n"
        );
    }

    #[test]
    fn crlf_changes_are_not_normalized_away() {
        let preview = preview(b"a\r\n", b"a\n", target()).expect("CRLF change");

        assert!(preview.changed);
        assert_eq!(preview.diff, "--- current\n+++ proposed\n-a\r\n+a\n");
    }

    #[test]
    fn binary_and_oversized_inputs_are_rejected() {
        assert_eq!(
            preview(&[0xff], b"text", target()).expect_err("binary current"),
            EditError::CurrentNotText
        );
        assert_eq!(
            preview(b"text", &[0xff], target()).expect_err("binary proposed"),
            EditError::ProposedNotText
        );
        let oversized = vec![b'a'; MAX_EDIT_CONTENT_BYTES + 1];
        assert_eq!(
            preview(&oversized, b"text", target()).expect_err("oversized current"),
            EditError::CurrentTooLarge
        );
        assert_eq!(
            preview(b"text", &oversized, target()).expect_err("oversized proposed"),
            EditError::ProposedTooLarge
        );
    }

    #[test]
    fn excessive_line_counts_are_rejected() {
        let current = "old\n".repeat(MAX_EDIT_LINES + 1);
        let proposed = "new\n".repeat(MAX_EDIT_LINES + 1);

        assert_eq!(
            preview(current.as_bytes(), proposed.as_bytes(), target())
                .expect_err("excessive lines"),
            EditError::TooManyLines
        );
    }

    #[test]
    fn proposed_content_reads_are_bounded_and_regular() {
        use std::fs;

        let directory = tempfile::tempdir().expect("private temporary directory");
        let path = directory.path().join("proposed.txt");
        fs::write(&path, b"content").expect("proposed file");

        assert_eq!(
            read_proposed_file(&path, 7).expect("bounded proposed content"),
            b"content"
        );
        assert_eq!(
            read_proposed_file(&path, 6).expect_err("oversized proposed content"),
            ProposedReadError::TooLarge
        );
        assert_eq!(
            read_proposed_file(directory.path(), 10).expect_err("directory source"),
            ProposedReadError::NotRegular
        );
        assert_eq!(
            read_proposed_file(&directory.path().join("missing"), 10).expect_err("missing source"),
            ProposedReadError::Io
        );
    }

    #[test]
    fn large_diffs_are_truncated_within_the_bound() {
        let mut current = String::from("keep\n");
        let mut proposed = String::from("keep\n");
        for _ in 0..20_000 {
            current.push_str("old line with some content\n");
            proposed.push_str("new line with some content\n");
        }
        let preview =
            preview(current.as_bytes(), proposed.as_bytes(), target()).expect("large diff");

        assert!(preview.changed);
        assert!(preview.truncated);
        assert!(preview.diff.len() <= MAX_DIFF_BYTES);
        assert!(preview.diff.ends_with(TRUNCATION_MARKER));
    }
}
