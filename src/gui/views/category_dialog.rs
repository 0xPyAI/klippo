//! The "Add new category" dialog (Options > Save to > New, "+" in Download
//! File Info, the Categories tree) and the category changes behind it.

use std::path::PathBuf;

use eframe::egui::{self, Color32, Context, RichText};

use super::classic_ui::{button, note, right_buttons};
use super::{dialog_window, fixed_width};
use crate::core::http;
use crate::gui::state::{save_state_to_disk, AppSettings, Category, CategoryFilter, KlippoApp, UserCategory};

/// Where the dialog was opened: the new category is selected there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CategoryFrom {
    AddDialog,
    Options,
    Tree,
}

#[derive(Debug, Clone)]
pub struct NewCategoryDialog {
    pub from: CategoryFrom,
    pub name: String,
    pub types: String,
    pub dir: String,
    /// The folder follows the name until the user changes it.
    pub dir_edited: bool,
    pub error: Option<String>,
}

/// "epub, *.MOBI .azw3" -> "EPUB MOBI AZW3", as Options > Save to shows types.
pub fn normalize_types(text: &str) -> String {
    text.split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .map(|t| t.trim().trim_start_matches("*.").trim_start_matches('.').to_uppercase())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

impl KlippoApp {
    pub fn open_new_category(&mut self, from: CategoryFrom) {
        self.new_category = Some(NewCategoryDialog {
            from,
            name: String::new(),
            types: String::new(),
            dir: self.settings.default_dir.to_string_lossy().to_string(),
            dir_edited: false,
            error: None,
        });
    }

    /// Adds a category with its file types and folder.
    pub fn add_category(&mut self, name: &str, types: &str, dir: PathBuf) -> Result<Category, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("Type a name for the category".to_string());
        }
        if Category::all().iter().any(|c| c.name().eq_ignore_ascii_case(name)) {
            return Err(format!("There is already a category \"{}\"", name));
        }
        let id = self.settings.user_categories.iter().map(|c| c.id).max().unwrap_or(0) + 1;
        let cat = Category::User(id);
        let dir = if dir.as_os_str().is_empty() { self.settings.default_dir.join(http::sanitize_filename(name)) } else { dir };
        let (name, types) = (name.to_string(), normalize_types(types));
        self.edit_categories(|s| {
            s.user_categories.push(UserCategory { id, name: name.clone() });
            s.category_types.insert(cat, types.clone());
            s.category_dirs.insert(cat, dir.clone());
        });
        Ok(cat)
    }

    /// Deletes a category the user added; its files go to the category of their file type.
    pub fn delete_category(&mut self, cat: Category) {
        let Category::User(id) = cat else { return };
        self.edit_categories(|s| {
            s.user_categories.retain(|c| c.id != id);
            s.category_types.remove(&cat);
            s.category_dirs.remove(&cat);
        });
        for d in self.downloads.iter_mut().filter(|d| d.category == cat) {
            d.category = Category::from_filename(&d.filename);
        }
        if matches!(self.selected_category, CategoryFilter::Category(c) | CategoryFilter::UnfinishedIn(c) | CategoryFilter::FinishedIn(c) if c == cat) {
            self.selected_category = CategoryFilter::All;
        }
        if self.options_save_to_category == cat {
            self.options_save_to_category = Category::General;
        }
        if self.add.category == cat {
            self.add.category = Category::General;
        }
        self.save_downloads();
    }

    /// A category change is kept at once, also when the Options dialog is
    /// open and its Cancel puts the other settings back.
    fn edit_categories(&mut self, edit: impl Fn(&mut AppSettings)) {
        edit(&mut self.settings);
        if let Some(backup) = self.settings_backup.as_mut() {
            edit(backup);
        }
        Category::set_user_categories(&self.settings.user_categories);
        Category::set_custom_types(&self.settings.category_types);
        save_state_to_disk(&self.downloads, self.settings_backup.as_ref().unwrap_or(&self.settings));
    }
}

pub fn render_new_category_dialog(app: &mut KlippoApp, ctx: &Context) {
    let Some(mut dlg) = app.new_category.take() else { return };
    let mut is_open = true;
    let (mut ok, mut cancel) = (false, false);
    dialog_window("Add new category", ctx, 366.0)
        .id(egui::Id::new("new_category_dialog"))
        .open(&mut is_open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_width(366.0);
            ui.set_max_width(366.0);
            ui.label("Category name");
            if fixed_width(ui, f32::INFINITY, |ui| ui.add(egui::TextEdit::singleline(&mut dlg.name).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY))).changed() {
                dlg.error = None;
            }
            ui.add_space(4.0);
            ui.label("Automatically put in this category the following file types:");
            fixed_width(ui, f32::INFINITY, |ui| ui.add(egui::TextEdit::singleline(&mut dlg.types).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
            note(ui, "Separate the types with spaces, for example: EPUB MOBI AZW3");
            ui.add_space(4.0);
            ui.label("Save the files of this category to");
            ui.horizontal(|ui| {
                if fixed_width(ui, ui.available_width() - 78.0, |ui| ui.add(egui::TextEdit::singleline(&mut dlg.dir).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY))).changed() {
                    dlg.dir_edited = true;
                }
                if button(ui, true, "Browse").clicked() {
                    if let Some(folder) = rfd::FileDialog::new().set_directory(&dlg.dir).pick_folder() {
                        dlg.dir = folder.to_string_lossy().to_string();
                        dlg.dir_edited = true;
                    }
                }
            });
            if let Some(ref e) = dlg.error {
                ui.label(RichText::new(e).color(Color32::from_rgb(220, 53, 69)));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                right_buttons(ui, |ui| {
                    cancel = button(ui, true, "Cancel").clicked();
                    ok = button(ui, true, "OK").clicked();
                });
            });
        });
    if !dlg.dir_edited {
        let name = http::sanitize_filename(dlg.name.trim());
        let base = app.settings.default_dir.clone();
        dlg.dir = (if name.is_empty() { base } else { base.join(name) }).to_string_lossy().to_string();
    }
    if ok {
        match app.add_category(&dlg.name, &dlg.types, PathBuf::from(dlg.dir.trim())) {
            Ok(cat) => {
                match dlg.from {
                    CategoryFrom::AddDialog => {
                        app.add.category = cat;
                        app.add.dir_edited = false;
                        app.update_add_dir_for_category();
                    }
                    CategoryFrom::Options => app.options_save_to_category = cat,
                    CategoryFrom::Tree => app.selected_category = CategoryFilter::Category(cat),
                }
                app.set_status(format!("Added the category \"{}\"", cat.name()));
                return;
            }
            Err(e) => dlg.error = Some(e),
        }
    }
    if !(cancel || !is_open) {
        app.new_category = Some(dlg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_are_written_as_options_shows_them() {
        assert_eq!(normalize_types("epub, *.MOBI .azw3;  pdf"), "EPUB MOBI AZW3 PDF");
        assert_eq!(normalize_types("   "), "");
    }
}
