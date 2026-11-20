//! Safe staged extraction of directory trees.

use crate::filesystem::DirectoryEntry;
use anyhow::{Context, Result, anyhow, bail};
use std::fs;
use std::path::Path;

pub(crate) const MAX_TREE_ENTRIES: u64 = 1_000_000;
pub(crate) const MAX_TREE_DEPTH: usize = 256;

#[derive(Debug, Clone, Default)]
pub struct RecursiveExtraction {
    pub files: u64,
    pub directories: u64,
    pub logical_bytes: u64,
    /// Symlinks and unsupported special entries are reported but never followed.
    pub skipped_entries: u64,
    pub report_name: String,
}

pub(crate) fn source_child(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{}/{name}", parent.trim_end_matches('/'))
    }
}

pub(crate) fn validate_component(name: &str) -> Result<()> {
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        bail!("backup entry name {name:?} cannot be represented safely as one path component");
    }
    #[cfg(windows)]
    {
        let invalid = ['<', '>', ':', '"', '|', '?', '*'];
        if name.chars().any(|character| invalid.contains(&character)) || name.ends_with(['.', ' '])
        {
            bail!("backup entry name {name:?} is not representable on Windows");
        }
        let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
        if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        {
            bail!("backup entry name {name:?} is a reserved Windows device name");
        }
    }
    Ok(())
}

/// Extract a source directory into a staged sibling and publish it only after
/// every regular file succeeds. A forced replacement keeps the previous tree
/// in the staging directory until the new tree has been renamed into place.
pub(crate) fn extract_directory_tree<List, Extract>(
    backup_file: Option<&Path>,
    source: &str,
    source_root: &str,
    destination: &Path,
    force: bool,
    mut list: List,
    mut extract: Extract,
) -> Result<RecursiveExtraction>
where
    List: FnMut(&str) -> Result<Vec<DirectoryEntry>>,
    Extract: FnMut(&str, &Path) -> Result<u64>,
{
    let publication =
        crate::destination::RestoreDestination::prepare(backup_file, destination, force, false)?;
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .with_context(|| format!("creating destination parent {}", parent.display()))?;
    let staging = tempfile::Builder::new()
        .prefix(".zfse-tree-")
        .tempdir_in(parent)
        .with_context(|| {
            format!(
                "creating extraction staging directory in {}",
                parent.display()
            )
        })?;
    let staged_root = staging.path().join("recovered");
    fs::create_dir(&staged_root)?;

    let mut result = RecursiveExtraction {
        directories: 1,
        ..RecursiveExtraction::default()
    };
    let mut report = crate::report::ReportWriter::new(staging.path(), source, source_root)?;
    let mut stack = vec![(source_root.to_owned(), staged_root.clone(), 0usize)];
    while let Some((source_directory, target_directory, depth)) = stack.pop() {
        if depth > MAX_TREE_DEPTH {
            bail!("backup directory tree exceeds the {MAX_TREE_DEPTH}-level safety limit");
        }
        for entry in list(&source_directory)? {
            crate::progress::checkpoint()?;
            let total = result
                .files
                .saturating_add(result.directories)
                .saturating_add(result.skipped_entries);
            if total >= MAX_TREE_ENTRIES {
                bail!("backup directory tree exceeds the {MAX_TREE_ENTRIES}-entry safety limit");
            }
            validate_component(&entry.name)?;
            let source_path = source_child(&source_directory, &entry.name);
            let target_path = target_directory.join(&entry.name);
            let relative = target_path
                .strip_prefix(&staged_root)?
                .components()
                .map(|p| p.as_os_str().to_str().context("invalid name"))
                .collect::<Result<Vec<_>>>()?
                .join("/");
            let mut expected_bytes = 0;
            match entry.dirent_type {
                4 => {
                    fs::create_dir(&target_path).with_context(|| {
                        format!("creating recovered directory {}", target_path.display())
                    })?;
                    result.directories += 1;
                    stack.push((source_path, target_path.clone(), depth + 1));
                }
                8 => {
                    let bytes = extract(&source_path, &target_path).with_context(|| {
                        format!("extracting {source_path:?} to {}", target_path.display())
                    })?;
                    expected_bytes = bytes;
                    crate::progress::completed_file();
                    result.files += 1;
                    result.logical_bytes = result
                        .logical_bytes
                        .checked_add(bytes)
                        .ok_or_else(|| anyhow!("recursive extraction byte count overflows"))?;
                }
                _ => result.skipped_entries += 1,
            }
            report.entry(&relative, &target_path, entry.dirent_type, expected_bytes)?;
        }
    }

    result.report_name = report.finish(&staged_root)?;
    publication.directory(&staged_root)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::extract_directory_tree;
    use crate::filesystem::DirectoryEntry;
    use std::fs;

    #[test]
    fn recursive_extraction_stages_and_replaces_a_complete_tree() {
        let temporary = tempfile::tempdir().unwrap();
        let destination = temporary.path().join("recovered");
        let result = extract_directory_tree(
            None,
            "test snapshot",
            "/Users",
            &destination,
            false,
            |path| match path {
                "/Users" => Ok(vec![DirectoryEntry {
                    name: "Alice".to_owned(),
                    object_id: 1,
                    dirent_type: 4,
                    logical_size: None,
                }]),
                "/Users/Alice" => Ok(vec![DirectoryEntry {
                    name: "hello.txt".to_owned(),
                    object_id: 2,
                    dirent_type: 8,
                    logical_size: Some(5),
                }]),
                _ => unreachable!(),
            },
            |path, output| {
                assert_eq!(path, "/Users/Alice/hello.txt");
                fs::write(output, b"hello").unwrap();
                Ok(5)
            },
        )
        .unwrap();
        assert_eq!(result.files, 1);
        assert_eq!(result.directories, 2);
        assert_eq!(result.logical_bytes, 5);
        assert_eq!(
            fs::read(destination.join("Alice/hello.txt")).unwrap(),
            b"hello"
        );

        fs::write(destination.join("old.txt"), b"old").unwrap();
        extract_directory_tree(
            None,
            "test snapshot",
            "/empty",
            &destination,
            true,
            |_| Ok(Vec::new()),
            |_, _| unreachable!(),
        )
        .unwrap();
        assert!(!destination.join("old.txt").exists());
    }

    #[test]
    fn recursive_failure_leaves_an_existing_destination_unchanged() {
        let temporary = tempfile::tempdir().unwrap();
        let destination = temporary.path().join("recovered");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("old.txt"), b"old").unwrap();

        let result = extract_directory_tree(
            None,
            "test snapshot",
            "/",
            &destination,
            true,
            |_| {
                Ok(vec![DirectoryEntry {
                    name: "new.txt".to_owned(),
                    object_id: 1,
                    dirent_type: 8,
                    logical_size: Some(3),
                }])
            },
            |_, _| anyhow::bail!("synthetic extraction failure"),
        );
        assert!(result.is_err());
        assert_eq!(fs::read(destination.join("old.txt")).unwrap(), b"old");
        assert!(!destination.join("new.txt").exists());
    }
}

#[cfg(test)]
mod report_tests {
    use super::*;
    #[test]
    fn reports_files_empty_folders_and_skipped_links_and_detects_changes() {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("restored");
        let entry = |name: &str, kind| DirectoryEntry {
            name: name.into(),
            object_id: 1,
            dirent_type: kind,
            logical_size: None,
        };
        let result = extract_directory_tree(
            None,
            "tank/host@day1",
            "/docs",
            &dest,
            false,
            |p| {
                Ok(if p == "/docs" {
                    vec![
                        entry("empty", 4),
                        entry("link", 10),
                        entry("ZFS Restore Report.jsonl", 8),
                        entry("ZFS Restore Report (1).jsonl", 10),
                    ]
                } else {
                    vec![]
                })
            },
            |_, p| {
                fs::write(p, b"document")?;
                Ok(8)
            },
        )
        .unwrap();
        assert_eq!(result.report_name, "ZFS Restore Report (2).jsonl");
        let report = dest.join(result.report_name);
        assert_eq!(crate::report::verify(&report).unwrap(), 4);
        let records: Vec<serde_json::Value> = fs::read_to_string(&report)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(records[0]["source"], "tank/host@day1");
        assert!(records.iter().any(|v| v["kind"] == "skipped"
            && v["path"] == "link"
            && v["reason"] == "symbolic link"));
        fs::write(dest.join("ZFS Restore Report.jsonl"), b"changed").unwrap();
        assert!(
            crate::report::verify(&report)
                .unwrap_err()
                .to_string()
                .contains("ZFS Restore Report.jsonl")
        );
    }
}
