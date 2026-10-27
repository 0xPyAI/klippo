pub mod menu_bar;
pub mod toolbar;
pub mod category_sidebar;
pub mod downloads_table;
pub mod status_bar;
pub mod add_url_modal;
pub mod progress_modal;
pub mod complete_modal;
pub mod options_modal;
pub mod scheduler_modal;
pub mod speed_limiter_modal;
pub mod file_properties_modal;
pub mod batch_download_modal;
pub mod about_modal;
pub mod dialogs;
pub mod classic_ui;
pub mod find_modal;
pub mod category_dialog;
pub mod site_grabber_modal;

use std::sync::{Arc, OnceLock};

use eframe::egui::{self, Context, Pos2, Ui, UiBuilder, Vec2};

/// Space between a dialog's border and its controls.
const DIALOG_MARGIN: f32 = 8.0;

/// A dialog in a window of its own: it can be moved outside the
/// main window. It takes the same builder calls as `egui::Window`.
pub struct Dialog<'open> {
    title: String,
    width: f32,
    id: egui::Id,
    open: Option<&'open mut bool>,
    offset: Vec2,
    front: bool,
    minimize: bool,
}

/// A dialog `width` points wide, centered on the main window when it opens.
pub fn dialog_window(title: impl Into<String>, _ctx: &Context, width: f32) -> Dialog<'static> {
    let title = title.into();
    Dialog { id: egui::Id::new(("dialog", title.clone())), title, width, open: None, offset: Vec2::ZERO, front: false, minimize: false }
}

/// Like `dialog_window`, offset so several windows do not stack exactly.
pub fn dialog_window_offset(title: impl Into<String>, ctx: &Context, index: usize, width: f32) -> Dialog<'static> {
    let mut dialog = dialog_window(title, ctx, width);
    dialog.offset = Vec2::splat(24.0 * (index % 8) as f32);
    dialog
}

impl<'open> Dialog<'open> {
    pub fn id(mut self, id: egui::Id) -> Self {
        self.id = id;
        self
    }

    /// Clears `open` when the user closes the window with its close box.
    pub fn open<'a>(self, open: &'a mut bool) -> Dialog<'a> {
        Dialog { title: self.title, width: self.width, id: self.id, open: Some(open), offset: self.offset, front: self.front, minimize: self.minimize }
    }

    /// A minimize button in the title bar (on Download File Info,
    /// the progress and complete windows and Download all links); the
    /// minimized window waits on the taskbar.
    pub fn minimizable(mut self, minimize: bool) -> Self {
        self.minimize = minimize;
        self
    }

    /// Opened for another program (the browser, the clipboard, the command
    /// line): the window comes in front of that program and takes the keyboard
    /// once it shows. Only read when the dialog opens.
    pub fn bring_to_front(mut self, front: bool) -> Self {
        self.front = front;
        self
    }

    /// Dialogs keep the size of their contents.
    pub fn resizable(self, _resizable: bool) -> Self {
        self
    }

    /// A window of its own is always above the main window's contents.
    pub fn order(self, _order: egui::Order) -> Self {
        self
    }

    /// Dialogs open centered on the main window.
    pub fn default_pos(self, _pos: impl Into<Pos2>) -> Self {
        self
    }

    pub fn default_width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    pub fn max_width(self, _width: f32) -> Self {
        self
    }

    pub fn collapsible(self, _collapsible: bool) -> Self {
        self
    }

    /// Shows the dialog this frame; the window fits its contents.
    pub fn show<R>(self, ctx: &Context, add: impl FnOnce(&mut Ui) -> R) -> Option<R> {
        let Dialog { title, width, id, open, offset, front, minimize } = self;
        if open.as_deref() == Some(&false) {
            return None;
        }
        let state_id = id.with("native_dialog");
        let pass = ctx.cumulative_pass_nr();
        // Top-left corner, size, the pass it was last shown in, the passes since
        // it opened and whether it comes in front of other programs.
        let (pos, size, last, age, to_front): (Option<Pos2>, Vec2, u64, u32, bool) =
            ctx.data(|d| d.get_temp(state_id)).unwrap_or((None, Vec2::new(width + 2.0 * DIALOG_MARGIN, 160.0), 0, 0, false));
        let opening = pos.is_none() || last + 1 < pass;
        let age = if opening { 0 } else { age.saturating_add(1) };
        let to_front = if opening { front } else { to_front };
        let pos = match pos {
            Some(p) if !opening => p,
            _ => {
                // A minimized (or tray) main window sits far off screen: then use the screen center.
                let main = ctx
                    .input(|i| if i.viewport().minimized == Some(true) { None } else { i.viewport().outer_rect })
                    .filter(|r| r.min.x > -10000.0 && r.min.y > -10000.0);
                let center = match main {
                    Some(r) => r.center(),
                    None => ctx.input(|i| i.viewport().monitor_size).map(|s| (s / 2.0).to_pos2()).unwrap_or(Pos2::new(640.0, 400.0)),
                };
                (center - Vec2::new(size.x / 2.0, size.y / 2.0 + 40.0) + offset).max(Pos2::new(8.0, 8.0))
            }
        };
        let builder = egui::ViewportBuilder::default()
            .with_title(title.clone())
            .with_position(pos)
            .with_inner_size(size)
            // Hidden for the first frames, while it takes the size of its contents.
            .with_visible(age >= 2)
            .with_resizable(false)
            .with_minimize_button(minimize)
            .with_maximize_button(false)
            .with_icon(dialog_icon());
        let dark = crate::gui::theme::current(ctx).dark;
        let mut add = Some(add);
        let mut out = None;
        let mut closed = false;
        let mut used: Option<Vec2> = None;
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of(id), builder, |vctx, class| {
            if class == egui::ViewportClass::Embedded {
                // No separate windows here: show it inside the main window.
                let mut still_open = true;
                egui::Window::new(title.clone()).id(id).collapsible(false).resizable(false).open(&mut still_open).show(vctx, |ui| {
                    if let Some(add) = add.take() {
                        out = Some(add(ui));
                    }
                });
                closed = !still_open;
                return;
            }
            if to_front && age == 3 && !crate::gui::system::test_log(&format!("front {}", title)) {
                // Shown since the last frame: in front of the browser, with the keyboard.
                vctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            if age < 10 {
                // Windows draws a dark title bar when apps are dark; follow Klippo's look.
                // (Sent for a few frames: the window may not exist yet in the first one.)
                vctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(if dark { egui::SystemTheme::Dark } else { egui::SystemTheme::Light }));
            }
            let fill = vctx.style().visuals.window_fill;
            // Minimized, the window has no size: keep the one it had.
            let minimized = vctx.input(|i| i.viewport().minimized == Some(true));
            egui::CentralPanel::default().frame(egui::Frame::none().fill(fill).inner_margin(DIALOG_MARGIN)).show(vctx, |ui| {
                // The panel fills the window; the contents' own size sets the window size.
                let contents = ui.vertical(|ui| {
                    if let Some(add) = add.take() {
                        out = Some(add(ui));
                    }
                });
                if !minimized {
                    used = Some(contents.response.rect.size());
                }
            });
            closed = vctx.input(|i| i.viewport().close_requested());
        });
        if matches!(age, 1 | 2 | 5) {
            // The window exists now: give its title bar Klippo's color.
            crate::gui::system::set_title_bar_theme(&title, dark);
        }
        if age < 3 {
            ctx.request_repaint();
        }
        let mut size = size;
        if let Some(used) = used {
            let want = Vec2::new(width.max(used.x), used.y) + Vec2::splat(2.0 * DIALOG_MARGIN);
            let want = Vec2::new(want.x.ceil(), want.y.ceil());
            if (want - size).abs().max_elem() > 0.5 {
                size = want;
                ctx.request_repaint();
            }
        }
        ctx.data_mut(|d| d.insert_temp(state_id, (Some(pos), size, pass, age, to_front)));
        if closed {
            if let Some(open) = open {
                *open = false;
            }
        }
        out
    }
}

/// Klippo's icon for the dialog windows (made once).
fn dialog_icon() -> Arc<egui::IconData> {
    static ICON: OnceLock<Arc<egui::IconData>> = OnceLock::new();
    ICON.get_or_init(|| Arc::new(crate::gui::app_icon())).clone()
}

/// Runs `add` in a box of exactly `width` (f32::INFINITY = the rest of the row).
///
/// egui 0.30 allocates extra width after a single-line TextEdit whose text is
/// longer than the box; inside this child area that extra space cannot push
/// later widgets to the right or widen the dialog.
pub fn fixed_width<R>(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let width = if width.is_finite() { width } else { ui.available_width() };
    let height = ui.spacing().interact_size.y.max(22.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::hover());
    let mut child = ui.new_child(UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
    add(&mut child)
}
