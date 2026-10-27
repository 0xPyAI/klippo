use std::path::{Path, PathBuf};

use eframe::egui::{self, Color32, Context, RichText};
use crate::core::resume::ResumeManager;
use super::classic_ui::{button, right_buttons};
use super::{dialog_window, fixed_width};
use crate::gui::icons::{self, SmallIcon};
use crate::gui::state::{Category, DownloadKind, DownloadStatus, HashAlgo, HashState, KlippoApp};
use crate::gui::system;

#[derive(Clone, Default)]
struct EditBuffer {
    id: u64,
    url: String,
    filename: String,
    description: String,
    referer: String,
    username: String,
    password: String,
    /// The folder chosen with "Browse..."; the file moves there with OK.
    folder: Option<PathBuf>,
    /// A checksum to verify the finished file against, and its algorithm.
    expected_hash: String,
    algo: HashAlgo,
}

impl KlippoApp {
    /// Renames a download (and its partial data) on disk.
    pub fn rename_item(&mut self, id: u64, new_name: &str) -> Result<(), String> {
        let clean = crate::core::http::sanitize_filename(new_name.trim());
        if clean.is_empty() {
            return Err("The file name is empty".to_string());
        }
        let Some(item) = self.item(id) else { return Err("The download is gone".to_string()) };
        if item.status.is_active() {
            return Err("Stop the download before renaming it".to_string());
        }
        if item.filename == clean {
            return Ok(());
        }
        let new_path = item.save_path.with_file_name(&clean);
        self.relocate(id, new_path)?;
        if let Some(item) = self.item_mut(id) {
            item.category = Category::from_filename(&item.filename);
            item.filename_auto = false;
        }
        self.save_downloads();
        Ok(())
    }

    /// Moves a download (and its partial data) to another folder, also on another drive.
    pub fn move_item(&mut self, id: u64, folder: &Path) -> Result<(), String> {
        let Some(item) = self.item(id) else { return Err("The download is gone".to_string()) };
        if item.status.is_active() {
            return Err("Stop the download before moving it".to_string());
        }
        let Some(name) = item.save_path.file_name() else { return Err("The download has no file name".to_string()) };
        let new_path = folder.join(name);
        if new_path == item.save_path {
            return Ok(());
        }
        std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
        self.relocate(id, new_path)?;
        if let Some(item) = self.item_mut(id) {
            // A place chosen earlier in "Download File Info" is replaced.
            item.move_to = None;
        }
        self.save_downloads();
        Ok(())
    }

    /// Puts the file and its partial data (resume state, stream parts) at
    /// `new_path` and changes the list item.
    fn relocate(&mut self, id: u64, new_path: PathBuf) -> Result<(), String> {
        let Some(item) = self.item(id) else { return Err("The download is gone".to_string()) };
        let (old_path, kind) = (item.save_path.clone(), item.kind);
        if new_path.exists() {
            return Err(format!("{} already exists", new_path.display()));
        }
        let partial_data = |p: &Path| match kind {
            DownloadKind::Http => Some(ResumeManager::state_path_for(p)),
            DownloadKind::Youtube => Some(crate::core::youtube::YoutubeGrabber::parts_dir_for(p)),
            DownloadKind::Hls => Some(crate::core::hls::HLSGrabber::temp_dir_for(p)),
            DownloadKind::Ftp => None,
        };
        if old_path.exists() {
            move_path(&old_path, &new_path).map_err(|e| e.to_string())?;
        }
        if let (Some(old_data), Some(new_data)) = (partial_data(&old_path), partial_data(&new_path)) {
            if old_data.exists() && old_data != new_data {
                if let Err(e) = move_path(&old_data, &new_data) {
                    // The file goes back: a download and its partial data stay together.
                    let _ = move_path(&new_path, &old_path);
                    return Err(e.to_string());
                }
            }
        }
        if let Some(item) = self.item_mut(id) {
            item.filename = new_path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            item.save_path = new_path;
        }
        Ok(())
    }
}

/// Renames `from`, or copies it and deletes it when `to` is on another drive.
pub fn move_path(from: &Path, to: &Path) -> std::io::Result<()> {
    move_path_using(from, to, |a, b| std::fs::rename(a, b))
}

/// `move_path` with the rename step given, so a test can be on "another drive".
fn move_path_using(from: &Path, to: &Path, rename: impl Fn(&Path, &Path) -> std::io::Result<()>) -> std::io::Result<()> {
    match rename(from, to) {
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            if from.is_dir() {
                copy_dir(from, to)?;
                std::fs::remove_dir_all(from)
            } else {
                std::fs::copy(from, to)?;
                std::fs::remove_file(from)
            }
        }
        other => other,
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

pub fn render_file_properties_modal(app: &mut KlippoApp, ctx: &Context) {
    let Some(target_id) = app.show_file_properties_dialog else { return };
    let Some(item) = app.item(target_id).cloned() else {
        app.show_file_properties_dialog = None;
        return;
    };

    let buffer_id = egui::Id::new("properties_edit_buffer");
    let mut buffer: EditBuffer = ctx.data_mut(|d| d.get_temp::<EditBuffer>(buffer_id)).filter(|b| b.id == target_id).unwrap_or_else(|| EditBuffer {
        id: target_id,
        url: item.url.clone(),
        filename: item.filename.clone(),
        description: item.description.clone(),
        referer: item.referer.clone().unwrap_or_default(),
        username: item.username.clone().unwrap_or_default(),
        password: item.password.clone().unwrap_or_default(),
        folder: None,
        expected_hash: item.expected_hash.clone(),
        algo: item.hash_algo,
    });

    let mut is_open = true;
    let mut close = false;
    let mut save = false;
    let mut compute_hash = false;
    let editable = !item.status.is_active();

    dialog_window("File Properties", ctx, 429.0)
        .id(egui::Id::new("file_properties_dialog"))
        .open(&mut is_open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_width(429.0);
            ui.set_max_width(429.0);
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(34.0), egui::Sense::hover());
                icons::small(ui.painter(), rect, SmallIcon::Category(item.category));
                fixed_width(ui, f32::INFINITY, |ui| ui.add_enabled(editable, egui::TextEdit::singleline(&mut buffer.filename).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
            });
            ui.add_space(4.0);
            let ext = Path::new(&item.filename).extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
            egui::Grid::new("prop_grid").num_columns(2).spacing([14.0, 6.0]).show(ui, |ui| {
                ui.label("Type:");
                ui.label(if ext.is_empty() { format!("{} file", item.category.name()) } else { format!("{} file ({})", ext, item.category.name()) });
                ui.end_row();
                ui.label("Status:");
                ui.label(item.status_string());
                ui.end_row();
                ui.label("Size:");
                ui.label(item.size_string());
                ui.end_row();
                ui.label("Save To:");
                ui.horizontal(|ui| {
                    let current = item.save_path.parent().map(Path::to_path_buf).unwrap_or_default();
                    let path = buffer.folder.as_ref().unwrap_or(&current).to_string_lossy().to_string();
                    fixed_width(ui, 262.0, |ui| ui.add(egui::Label::new(path.as_str()).truncate()).on_hover_text(path.as_str()));
                    let browse = button(ui, editable, "Browse...")
                        .on_hover_text("Choose another folder; the file moves there when you click OK")
                        .on_disabled_hover_text("Stop the download before moving it");
                    if browse.clicked() {
                        if let Some(folder) = rfd::FileDialog::new().set_directory(&current).pick_folder() {
                            buffer.folder = Some(folder);
                        }
                    }
                });
                ui.end_row();
                ui.label("Address:");
                fixed_width(ui, 338.0, |ui| ui.add_enabled(editable, egui::TextEdit::singleline(&mut buffer.url).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)))
                    .on_hover_text("Change this when a download link has expired");
                ui.end_row();
                ui.label("Description:");
                fixed_width(ui, 338.0, |ui| ui.add(egui::TextEdit::singleline(&mut buffer.description).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
                ui.end_row();
            });
            ui.add_space(4.0);
            ui.label("The web page from which this file was obtained:");
            egui::Grid::new("prop_grid2").num_columns(2).spacing([14.0, 6.0]).show(ui, |ui| {
                ui.label("Referer:");
                fixed_width(ui, 338.0, |ui| ui.add(egui::TextEdit::singleline(&mut buffer.referer).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
                ui.end_row();
                ui.label("Login");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut buffer.username).margin(crate::gui::theme::EDIT_MARGIN).desired_width(120.0));
                    ui.add_space(12.0);
                    ui.label("Password");
                    ui.add(egui::TextEdit::singleline(&mut buffer.password).margin(crate::gui::theme::EDIT_MARGIN).password(true).desired_width(120.0));
                });
                ui.end_row();
                ui.label("Last try");
                ui.horizontal(|ui| {
                    ui.label(item.last_try_string());
                    ui.add_space(20.0);
                    let result = match (&item.error_message, item.status) {
                        (Some(e), DownloadStatus::Failed) => format!("Result: {}", e),
                        (_, status) => format!("Result: {}", status.display_text()),
                    };
                    ui.add(egui::Label::new(result).truncate());
                });
                ui.end_row();
                ui.label("Checksum");
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("prop_algo").width(92.0).selected_text(buffer.algo.label()).show_ui(ui, |ui| {
                        for a in HashAlgo::all() {
                            ui.selectable_value(&mut buffer.algo, a, a.label());
                        }
                    });
                    ui.add_space(6.0);
                    ui.label("Expected");
                    fixed_width(ui, 150.0, |ui| {
                        ui.add(egui::TextEdit::singleline(&mut buffer.expected_hash).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY))
                            .on_hover_text("Paste a checksum here to verify the finished file")
                    });
                });
                ui.end_row();
                ui.label("");
                let hash_state = app.file_hashes.get(&target_id).cloned();
                ui.horizontal(|ui| match hash_state {
                    Some(HashState::Done(hash)) => {
                        fixed_width(ui, 206.0, |ui| ui.add(egui::Label::new(RichText::new(hash.as_str()).monospace().size(10.5)).truncate()).on_hover_text(hash.as_str()));
                        if ui.small_button("Copy").clicked() {
                            app.copy_to_clipboard(&hash);
                        }
                        let expected = buffer.expected_hash.trim().to_lowercase();
                        if !expected.is_empty() {
                            if hash.trim().to_lowercase() == expected {
                                ui.label(RichText::new("match").color(Color32::from_rgb(40, 160, 70)));
                            } else {
                                ui.label(RichText::new("mismatch").color(Color32::from_rgb(220, 53, 69)));
                            }
                        }
                    }
                    Some(HashState::Computing) => {
                        ui.spinner();
                        ui.label("Calculating...");
                    }
                    Some(HashState::Failed(e)) => {
                        ui.label(RichText::new(e).color(Color32::from_rgb(220, 53, 69)));
                    }
                    None => {
                        let ready = item.status == DownloadStatus::Completed && Path::new(&item.save_path).exists();
                        let verb = if buffer.expected_hash.trim().is_empty() { "Compute" } else { "Compute and verify" };
                        if button(ui, ready, verb).on_disabled_hover_text("Available when the download is complete").clicked() {
                            compute_hash = true;
                        }
                    }
                });
                ui.end_row();
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let completed = item.status == DownloadStatus::Completed;
                if button(ui, completed, "Open").clicked() {
                    crate::gui::system::open_file(&item.save_path);
                }
                if button(ui, true, "Open folder").clicked() {
                    system::reveal_in_folder(&item.save_path);
                }
                right_buttons(ui, |ui| {
                    if button(ui, true, "Cancel").clicked() {
                        close = true;
                    }
                    if button(ui, true, "OK").clicked() {
                        save = true;
                    }
                });
            });
        });

    if compute_hash {
        if let Some(it) = app.item_mut(target_id) {
            it.expected_hash = buffer.expected_hash.trim().to_string();
            it.hash_algo = buffer.algo;
            it.verified = None;
        }
        app.file_hashes.insert(target_id, HashState::Computing);
        app.bridge.compute_hash(target_id, item.save_path.clone(), buffer.algo);
    }

    if save {
        let mut ok = true;
        if buffer.filename.trim() != item.filename {
            if let Err(e) = app.rename_item(target_id, &buffer.filename) {
                app.set_status(format!("Rename failed: {}", e));
                ok = false;
            }
        }
        if let Some(folder) = buffer.folder.clone() {
            match app.move_item(target_id, &folder) {
                Ok(()) => buffer.folder = None,
                Err(e) => {
                    app.set_status(format!("Move failed: {}", e));
                    ok = false;
                }
            }
        }
        if let Some(it) = app.item_mut(target_id) {
            let url = buffer.url.trim().to_string();
            if !url.is_empty() && url != it.url && !it.status.is_active() {
                it.url = url;
            }
            it.description = buffer.description.clone();
            it.referer = Some(buffer.referer.trim().to_string()).filter(|r| !r.is_empty());
            it.username = Some(buffer.username.clone()).filter(|u| !u.is_empty());
            it.password = it.username.as_ref().map(|_| buffer.password.clone());
            it.expected_hash = buffer.expected_hash.trim().to_string();
            it.hash_algo = buffer.algo;
        }
        app.save_downloads();
        if ok {
            close = true;
        }
    }

    if close || !is_open {
        app.show_file_properties_dialog = None;
        ctx.data_mut(|d| d.remove::<EditBuffer>(buffer_id));
    } else {
        ctx.data_mut(|d| d.insert_temp(buffer_id, buffer));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_and_folders_move() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.bin"), dir.path().join("other").join("a.bin"));
        std::fs::write(&a, b"data").unwrap();
        std::fs::create_dir_all(b.parent().unwrap()).unwrap();
        move_path(&a, &b).unwrap();
        assert!(!a.exists());
        assert_eq!(std::fs::read(&b).unwrap(), b"data");
        // Stream parts are a folder; a copy keeps its subfolders.
        let parts = dir.path().join("parts");
        std::fs::create_dir_all(parts.join("x")).unwrap();
        std::fs::write(parts.join("x").join("1.ts"), b"1").unwrap();
        copy_dir(&parts, &dir.path().join("copy")).unwrap();
        assert_eq!(std::fs::read(dir.path().join("copy").join("x").join("1.ts")).unwrap(), b"1");
        move_path(&parts, &dir.path().join("moved")).unwrap();
        assert!(!parts.exists() && dir.path().join("moved").join("x").join("1.ts").exists());
    }

    #[test]
    fn a_move_to_another_drive_copies_then_deletes() {
        // Windows' answer when the folders are on different drives.
        let across = |_: &Path, _: &Path| Err(std::io::Error::from(std::io::ErrorKind::CrossesDevices));
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.bin"), dir.path().join("b.bin"));
        std::fs::write(&a, b"data").unwrap();
        move_path_using(&a, &b, across).unwrap();
        assert!(!a.exists());
        assert_eq!(std::fs::read(&b).unwrap(), b"data");
        let (parts, moved) = (dir.path().join("parts"), dir.path().join("moved"));
        std::fs::create_dir_all(parts.join("x")).unwrap();
        std::fs::write(parts.join("x").join("1.ts"), b"1").unwrap();
        move_path_using(&parts, &moved, across).unwrap();
        assert!(!parts.exists());
        assert_eq!(std::fs::read(moved.join("x").join("1.ts")).unwrap(), b"1");
        // Another error (a file in use, no access) is not a reason to copy.
        let denied = |_: &Path, _: &Path| Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        std::fs::write(&a, b"x").unwrap();
        assert!(move_path_using(&a, &dir.path().join("c.bin"), denied).is_err());
        assert!(a.exists() && !dir.path().join("c.bin").exists());
    }
}
