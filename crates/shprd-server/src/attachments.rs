//! Files a person attaches to a chat message.
//!
//! An agent can only read a file without asking when the file is somewhere its
//! sandbox and permission rules already allow: a coordinator launched by shprd
//! runs with its working directory as its reach, and a path outside it makes
//! claude ask ("allow reading from …?") and codex or opencode refuse. So an
//! attachment is never left where the person picked it: it is **copied into the
//! agent's own working directory**, under `.shprd/attachments/`, the one folder
//! shprd already keeps out of git (see [`shprd_git::worktree::ensure_state_excluded`]),
//! and the message names the copy by its absolute path.
//!
//! The copy is a plain file with a name made of letters, digits, `.`, `_` and
//! `-`, so a path never needs quoting in any agent's input line.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::dto::ChatAttachmentDto;
use crate::error::ApiReject;

/// The folder, under the agent's working directory, attachments are copied to.
pub const DIR: &str = ".shprd/attachments";

/// The largest file one message may attach, in bytes.
pub const MAX_BYTES: u64 = 25 * 1024 * 1024;

/// The most files one message may attach.
pub const MAX_FILES: usize = 10;

/// Makes each message's folder distinct even within one millisecond.
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A file name safe to put in any agent's input line: letters, digits, `.`,
/// `_` and `-` only, never empty and never starting with a dot.
pub fn safe_name(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_start_matches(['.', '_']).trim_end_matches('_');
    let kept: String = trimmed.chars().take(80).collect();
    if kept.is_empty() {
        String::from("file")
    } else {
        kept
    }
}

/// Copies `items` into `cwd/.shprd/attachments/<message>/` and answers where
/// each copy is, in order.
///
/// A refusal names the file and why, and nothing is left behind: the message's
/// folder is removed when any file fails.
pub fn store(cwd: &Path, items: &[ChatAttachmentDto]) -> Result<Vec<PathBuf>, ApiReject> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    if items.len() > MAX_FILES {
        return Err(ApiReject::bad_request(format!(
            "a message takes at most {MAX_FILES} attachments; {} were sent. Nothing was sent",
            items.len()
        )));
    }
    if !cwd.is_dir() {
        return Err(ApiReject::refused(format!(
            "the agent's folder {} is not there, so an attachment has nowhere to go. Nothing was sent",
            cwd.display()
        )));
    }
    // The state folder is kept out of git before anything is written into it,
    // so an attachment can never be committed. A folder that is not a
    // repository has nothing to commit to; its own `.gitignore` is made when
    // shprd makes it one.
    if let Err(err) = shprd_git::worktree::ensure_state_excluded(cwd) {
        // In a repository that cannot be told to ignore the folder an attachment could
        // be committed, so nothing is copied; a folder that is no repository has
        // nothing to commit to.
        let in_repo = shprd_git::git::Git::new(cwd)
            .run(&["rev-parse", "--git-dir"])
            .is_ok();
        if in_repo {
            return Err(ApiReject::refused(format!(
                "git could not be told to ignore {DIR} ({err}), so an attachment could be \
                 committed. Nothing was sent"
            )));
        }
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    let folder = cwd.join(DIR).join(format!(
        "{stamp}-{}",
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&folder).map_err(|err| {
        ApiReject::internal(format!("could not make {}: {err}", folder.display()))
    })?;
    let mut stored = Vec::new();
    for (index, item) in items.iter().enumerate() {
        match copy_one(&folder, index, item) {
            Ok(path) => stored.push(path),
            Err(reject) => {
                let _ = fs::remove_dir_all(&folder);
                return Err(reject);
            }
        }
    }
    Ok(stored)
}

/// One file copied into the message's folder; `index` keeps two files of one
/// name apart.
fn copy_one(folder: &Path, index: usize, item: &ChatAttachmentDto) -> Result<PathBuf, ApiReject> {
    let source = Path::new(&item.path);
    if !source.is_absolute() {
        return Err(ApiReject::bad_request(format!(
            "attachment {:?} is not an absolute path. Nothing was sent",
            item.path
        )));
    }
    let meta = fs::metadata(source).map_err(|err| {
        ApiReject::bad_request(format!(
            "attachment {} cannot be read: {err}. Nothing was sent",
            source.display()
        ))
    })?;
    if !meta.is_file() {
        return Err(ApiReject::bad_request(format!(
            "attachment {} is not a file. Nothing was sent",
            source.display()
        )));
    }
    if meta.len() > MAX_BYTES {
        return Err(ApiReject::bad_request(format!(
            "attachment {} is {} MB; the most is {} MB. Nothing was sent",
            source.display(),
            meta.len() / (1024 * 1024),
            MAX_BYTES / (1024 * 1024)
        )));
    }
    let wanted = item
        .name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .or_else(|| source.file_name().and_then(|name| name.to_str()))
        .unwrap_or("file");
    let target = folder.join(format!("{}-{}", index + 1, safe_name(wanted)));
    fs::copy(source, &target).map_err(|err| {
        ApiReject::internal(format!(
            "could not copy attachment {}: {err}. Nothing was sent",
            source.display()
        ))
    })?;
    Ok(target)
}

/// Removes the copies of a message that was not sent (the host refused it), so a retry does not
/// leave the first try's files behind. Each copy sits alone in its message's folder.
pub fn discard(stored: &[PathBuf]) {
    for folder in stored.iter().filter_map(|path| path.parent()) {
        // Only a message's own folder under the attachments folder is ever removed.
        if !folder.to_string_lossy().contains(DIR) {
            continue;
        }
        let _ = fs::remove_dir_all(folder);
    }
}

/// The message the agent receives: the person's words, then the files as
/// absolute paths one to a line, in a form every agent opens with its own file
/// tools (claude shows an image path as an image).
pub fn compose(text: &str, files: &[PathBuf]) -> String {
    if files.is_empty() {
        return text.to_owned();
    }
    let mut out = String::new();
    if !text.is_empty() {
        out.push_str(text);
        out.push_str("\n\n");
    }
    out.push_str(if files.len() == 1 {
        "Attached file (open it with your file tools):"
    } else {
        "Attached files (open them with your file tools):"
    });
    for file in files {
        out.push('\n');
        out.push_str(&file.display().to_string());
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn item(path: &Path, name: Option<&str>) -> ChatAttachmentDto {
        ChatAttachmentDto {
            path: path.display().to_string(),
            name: name.map(str::to_owned),
        }
    }

    #[test]
    fn a_name_is_made_safe_for_any_input_line() {
        assert_eq!(safe_name("My Report (final).pdf"), "My_Report_final_.pdf");
        assert_eq!(safe_name("../../etc/passwd"), "etc_passwd");
        assert_eq!(safe_name(".hidden"), "hidden");
        assert_eq!(safe_name("   "), "file");
        assert_eq!(safe_name("日本語.png"), "png");
    }

    #[test]
    fn files_are_copied_inside_the_working_directory() {
        let cwd = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let a = src.path().join("notes one.txt");
        let b = src.path().join("pic.png");
        fs::write(&a, "hello").unwrap();
        fs::write(&b, [0x89, b'P', b'N', b'G']).unwrap();
        let stored = store(cwd.path(), &[item(&a, None), item(&b, Some("shot.png"))]).unwrap();
        assert_eq!(stored.len(), 2);
        for path in &stored {
            assert!(path.starts_with(cwd.path().join(DIR)), "{path:?}");
            assert!(path.is_file());
        }
        assert_eq!(fs::read_to_string(&stored[0]).unwrap(), "hello");
        assert!(stored[0].to_string_lossy().ends_with("1-notes_one.txt"));
        assert!(stored[1].to_string_lossy().ends_with("2-shot.png"));
    }

    #[test]
    fn a_bad_file_leaves_nothing_behind_and_says_which() {
        let cwd = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let good = src.path().join("ok.txt");
        fs::write(&good, "x").unwrap();
        let missing = src.path().join("gone.txt");
        let err = store(cwd.path(), &[item(&good, None), item(&missing, None)]).unwrap_err();
        let said = format!("{err:?}");
        assert!(said.contains("gone.txt"), "{said}");
        let left = fs::read_dir(cwd.path().join(DIR)).map_or(0, Iterator::count);
        assert_eq!(left, 0, "the message's folder was removed");
        let relative = ChatAttachmentDto {
            path: "rel/x.txt".into(),
            name: None,
        };
        assert!(store(cwd.path(), &[relative]).is_err());
        assert!(
            store(cwd.path(), &[item(src.path(), None)]).is_err(),
            "a folder"
        );
    }

    #[test]
    fn the_message_lists_each_file_by_path() {
        let files = vec![PathBuf::from("/r/.shprd/attachments/1/a.txt")];
        assert_eq!(compose("hi", &[]), "hi");
        assert_eq!(
            compose("what is this?", &files),
            "what is this?\n\nAttached file (open it with your file tools):\n/r/.shprd/attachments/1/a.txt"
        );
        assert_eq!(
            compose("", &files),
            "Attached file (open it with your file tools):\n/r/.shprd/attachments/1/a.txt"
        );
    }
}
