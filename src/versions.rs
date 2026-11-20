//! Reopen a selected regular file at the same path in snapshots of its dataset.
//! Each layer is resolved on the requested snapshot, including its partition.
use crate::client::{InceptionCatalog, SourceCatalog, ViewKind, parent_path};
use crate::filesystem::DirectoryEntry;
use crate::inception::VolumeInfo;
use anyhow::{Context, Result, ensure};
use serde::Serialize;

#[derive(Clone)]
pub struct ImageLocation {
    path: String,
    offset: u64,
    length: Option<u64>,
    volume: VolumeInfo,
}

impl ImageLocation {
    pub fn selected(image: &InceptionCatalog, selector: &str) -> Result<Self> {
        let volume = image
            .volumes
            .iter()
            .find(|v| v.selector == selector && v.filesystem.is_some())
            .context("Choose a readable partition to look up file versions")?;
        Ok(Self {
            path: image.path_in_parent.clone(),
            offset: image.image_offset,
            length: image.requested_length,
            volume: volume.clone(),
        })
    }

    fn volume_index(&self, image: &InceptionCatalog) -> Result<usize> {
        let mut matches = image.volumes.iter().enumerate().filter(|(_, v)| {
            v.filesystem == self.volume.filesystem
                && if let Some(guid) = &self.volume.partition_guid {
                    v.partition_guid.as_ref() == Some(guid)
                } else {
                    v.selector == self.volume.selector
                        && v.offset == self.volume.offset
                        && v.partition_type == self.volume.partition_type
                }
        });
        let (index, _) = matches
            .next()
            .context("The selected partition is unavailable in this restore point")?;
        ensure!(
            matches.next().is_none(),
            "The selected partition is ambiguous in this restore point"
        );
        Ok(index)
    }
}

#[derive(Clone)]
pub struct VersionPath {
    pub images: Vec<ImageLocation>,
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct FileVersion {
    pub index: usize,
    pub snapshot: String,
    pub created_at: Option<u64>,
    pub size: Option<u64>,
    pub issue: Option<String>,
}

pub struct OpenedVersion {
    pub images: Vec<(InceptionCatalog, usize)>,
    pub entries: Vec<DirectoryEntry>,
    pub size: Option<u64>,
}

impl SourceCatalog {
    pub fn file_versions(
        &self,
        current: usize,
        location: &VersionPath,
        key: Option<&[u8]>,
        agent_password: Option<&[u8]>,
    ) -> Result<Vec<FileVersion>> {
        let dataset = &self.view(current)?.dataset;
        let mut versions = Vec::new();
        for (index, view) in self
            .views
            .iter()
            .enumerate()
            .filter(|(_, v)| &v.dataset == dataset && v.kind != ViewKind::Current)
        {
            crate::progress::checkpoint()?;
            let (size, issue) =
                match self.open_file_version(current, index, location, key, agent_password) {
                    Ok(opened) => (opened.size, None),
                    Err(error) => (None, Some(format!("{error:#}"))),
                };
            versions.push(FileVersion {
                index,
                snapshot: view.snapshot.clone().unwrap_or_else(|| view.label.clone()),
                created_at: view.created_at,
                size,
                issue,
            });
        }
        crate::progress::checkpoint()?;
        Ok(versions)
    }

    pub fn open_file_version(
        &self,
        current: usize,
        target: usize,
        location: &VersionPath,
        key: Option<&[u8]>,
        agent_password: Option<&[u8]>,
    ) -> Result<OpenedVersion> {
        let view = self.view(target)?;
        ensure!(
            view.dataset == self.view(current)?.dataset && view.kind != ViewKind::Current,
            "Choose a restore point from the selected file's dataset"
        );
        let mut images: Vec<(InceptionCatalog, usize)> = Vec::with_capacity(location.images.len());
        for layer in &location.images {
            let image = if let Some((parent, index)) = images.last() {
                parent.inspect_child(
                    Some(&parent.volumes[*index].selector),
                    &layer.path,
                    layer.offset,
                    layer.length,
                )?
            } else {
                let password = if layer.path.to_ascii_lowercase().ends_with(".detto") {
                    agent_password
                } else {
                    None
                };
                self.inspect_inception_with_key_material(
                    target,
                    &layer.path,
                    key,
                    password,
                    None,
                    layer.offset,
                    layer.length,
                )?
            };
            let selected = layer.volume_index(&image)?;
            images.push((image, selected));
        }
        let directory = parent_path(&location.path);
        let mut entries = if let Some((image, index)) = images.last() {
            image.list_directory(Some(&image.volumes[*index].selector), &directory)?
        } else {
            self.list_directory_with_key_material(target, &directory, key)?
        };
        let name = location
            .path
            .rsplit('/')
            .next()
            .context("Select a file first")?;
        let entry = entries
            .iter_mut()
            .find(|e| e.name == name)
            .context("This path is absent from the restore point")?;
        ensure!(
            entry.dirent_type == 8,
            "This path is not a regular file in the restore point"
        );
        if let Some((image, index)) = images.last() {
            entry.logical_size =
                Some(image.file_length(&image.volumes[*index].selector, &location.path)?);
        }
        let size = entry.logical_size;
        Ok(OpenedVersion {
            images,
            entries,
            size,
        })
    }
}
