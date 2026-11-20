//! Restore point and file history dialogs.
use super::*;
use zfs_send_extract::versions::{FileVersion, ImageLocation, OpenedVersion, VersionPath};

pub(super) unsafe fn choose_dataset(owner: HWND, source: &SourceCatalog) -> Option<usize> {
    let choices = presentation::dataset_choices(&source.views);
    let names: Vec<_> = choices.keys().map(|s| s.to_string()).collect();
    let row = dialogs::choose_row(
        owner,
        "Choose a computer or dataset",
        "This backup contains several sets of files. Open one to see its latest restore point.",
        "Computer or dataset",
        &names,
    )?;
    choices.get(names[row].as_str()).copied()
}

pub(super) unsafe fn find_versions(state: &mut AppState) -> Result<()> {
    let source = state
        .catalog
        .clone()
        .context("Open a backup with restore points first")?;
    let view = selected_view(state).context("Choose a restore point first")?;
    let name = selected_index(state.controls.list)
        .and_then(|i| state.entries.get(i))
        .filter(|entry| entry.dirent_type == 8)
        .context("Select a regular file to check its other versions")?
        .name
        .clone();
    let mut images = Vec::new();
    for (i, frame) in state.inception.iter().enumerate() {
        let volume = if let Some(next) = state.inception.get(i + 1) {
            next.parent_volume
        } else {
            selected_volume_index(state)
        };
        let v = volume
            .and_then(|j| frame.catalog.volumes.get(j))
            .context("Choose a partition first")?;
        images.push(ImageLocation::selected(&frame.catalog, &v.selector)?);
    }
    let location = VersionPath {
        images,
        path: child_path(&state.current_path, &name)?,
    };
    let key = active_zfs_key(state);
    let password = state.agent_password.clone();
    set_busy(state, true, "Checking file versions in this dataset…");
    spawn_job(state, move || {
        let versions = source.file_versions(
            view,
            &location,
            key.as_ref().map(SecretValue::bytes),
            password.as_ref().map(SecretValue::bytes),
        )?;
        Ok(JobResult::FileVersions(location, versions))
    });
    Ok(())
}

pub(super) unsafe fn choose_version(
    state: &mut AppState,
    location: VersionPath,
    versions: &[FileVersion],
) {
    let Some(source) = state.catalog.clone() else {
        return;
    };
    let Some(current) = selected_view(state) else {
        return;
    };
    if versions.is_empty() {
        show_message(
            state.hwnd,
            DialogIcon::Information,
            "No restore points for this file",
            "This dataset contains only its current files.",
        );
        return;
    }
    let zone: SnapshotTimeZone = state
        .settings
        .snapshot_time_zone
        .parse()
        .unwrap_or_default();
    let rows = versions
        .iter()
        .map(|v| {
            let date = v
                .created_at
                .and_then(|t| zone.format_short(t))
                .unwrap_or_else(|| "Date unavailable".into());
            let size = if v.issue.is_some() {
                "Unavailable".to_owned()
            } else {
                v.size
                    .map(presentation::format_size)
                    .unwrap_or_else(|| "Size unknown".into())
            };
            format!("{date} · {} · {size}", v.snapshot)
        })
        .collect::<Vec<_>>();
    let point = loop {
        let Some(i) = dialogs::choose_row(
            state.hwnd,
            "Other versions of this file",
            &location.path,
            "Restore point",
            &rows,
        ) else {
            return;
        };
        let Some(version) = versions.get(i) else {
            return;
        };
        if let Some(issue) = &version.issue {
            show_message(
                state.hwnd,
                DialogIcon::Information,
                "This version is unavailable",
                issue,
            );
        } else {
            break version.index;
        }
    };
    let key = active_zfs_key(state);
    let password = state.agent_password.clone();
    set_busy(state, true, "Opening file version…");
    spawn_job(state, move || {
        let opened = source.open_file_version(
            current,
            point,
            &location,
            key.as_ref().map(SecretValue::bytes),
            password.as_ref().map(SecretValue::bytes),
        )?;
        Ok(JobResult::VersionOpened {
            index: point,
            location,
            opened,
        })
    });
}

pub(super) unsafe fn activate_version(
    state: &mut AppState,
    index: usize,
    location: VersionPath,
    opened: OpenedVersion,
) {
    let key = active_zfs_key(state);
    SendMessageW(state.controls.view, CB_SETCURSEL, index, 0);
    // The shared engine has authenticated this key on the selected snapshot.
    if let Some(mut key) = key {
        key.view = credential_view(state);
        store_zfs_key(state, key);
    }
    state.inception.clear();
    SendMessageW(state.controls.volume, CB_RESETCONTENT, 0, 0);
    let mut parent_volume = None;
    for (catalog, selected) in opened.images {
        let directory = parent_path(&catalog.path_in_parent);
        state.inception.push(ImageFrame {
            catalog,
            parent_path: Some(directory),
            parent_volume,
        });
        parent_volume = Some(selected);
    }
    if let Some(frame) = state.inception.last() {
        populate_volumes(state, &frame.catalog, parent_volume);
    }
    set_source_enabled(state, true);
    update_mode(state);
    show_directory(state, parent_path(&location.path), opened.entries);
    let name = location.path.rsplit('/').next();
    if let Some(row) = state
        .entries
        .iter()
        .position(|entry| Some(entry.name.as_str()) == name)
    {
        select_list_item(state.controls.list, row);
        update_selection_actions(state);
    }
}
