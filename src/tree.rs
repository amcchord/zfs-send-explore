//! Safe staged extraction of directory trees.

use crate::filesystem::DirectoryEntry;
use anyhow::{Context, Result, anyhow, bail};
use std::fs;
use std::path::Path;

const MAX_TREE_ENTRIES: u64 = 1_000_000;
const MAX_TREE_DEPTH: usize = 256;

#[derive(Debug, Clone, Default)]
pub struct RecursiveExtraction {
    pub files: u64,
    pub directories: u64,
    pub logical_bytes: u64,
    /// Symlinks and unsupported special entries are reported but never followed.
    pub skipped_entries: u64,
}

fn source_child(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{}/{name}", parent.trim_end_matches('/'))
    }
}

fn validate_component(name: &str) -> Result<()> {
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

fn path_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn remove_staged_path(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).with_context(|| {
        format!(
            "reading replaced destination metadata at {}",
            path.display()
        )
    })?;
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
    .with_context(|| format!("removing replaced destination staged at {}", path.display()))
}

/// Extract a source directory into a staged sibling and publish it only after
/// every regular file succeeds. A forced replacement keeps the previous tree
/// in the staging directory until the new tree has been renamed into place.
pub(crate) fn extract_directory_tree<List, Extract>(
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
    if path_exists(destination) && !force {
        bail!(
            "destination {} already exists (pass --force to replace it)",
            destination.display()
        );
    }
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
    let mut stack = vec![(source_root.to_owned(), staged_root.clone(), 0usize)];
    while let Some((source_directory, target_directory, depth)) = stack.pop() {
        if depth > MAX_TREE_DEPTH {
            bail!("backup directory tree exceeds the {MAX_TREE_DEPTH}-level safety limit");
        }
        for entry in list(&source_directory)? {
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
            match entry.dirent_type {
                4 => {
                    fs::create_dir(&target_path).with_context(|| {
                        format!("creating recovered directory {}", target_path.display())
                    })?;
                    result.directories += 1;
                    stack.push((source_path, target_path, depth + 1));
                }
                8 => {
                    let bytes = extract(&source_path, &target_path).with_context(|| {
                        format!("extracting {source_path:?} to {}", target_path.display())
                    })?;
                    result.files += 1;
                    result.logical_bytes = result
                        .logical_bytes
                        .checked_add(bytes)
                        .ok_or_else(|| anyhow!("recursive extraction byte count overflows"))?;
                }
                _ => result.skipped_entries += 1,
            }
        }
    }

    let previous = staging.path().join("previous");
    if path_exists(destination) {
        fs::rename(destination, &previous).with_context(|| {
            format!(
                "staging existing destination {} for replacement",
                destination.display()
            )
        })?;
    }
    if let Err(error) = fs::rename(&staged_root, destination) {
        if path_exists(&previous) {
            let _ = fs::rename(&previous, destination);
        }
        return Err(error)
            .with_context(|| format!("publishing recovered directory {}", destination.display()));
    }
    if path_exists(&previous) {
        remove_staged_path(&previous)?;
    }
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
