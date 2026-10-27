use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId, Rounding, Stroke, TextStyle, Vec2};

/// The text in dialogs, the tree, the list and the toolbar: Tahoma 8 pt.
pub const LIST_FONT: f32 = 10.67;

/// Page titles ("Default download settings"): a size larger than the text, not bold.
pub const TITLE_FONT: f32 = 11.7;

/// Single-line text boxes are 14 dialog units high (22 px at 100 %).
pub const EDIT_MARGIN: egui::Margin = egui::Margin { left: 4.0, right: 4.0, top: 4.5, bottom: 4.5 };

/// Colors of the classic look. Two sets: light (classic) and dark
/// (used when Windows apps are set to dark).
#[derive(Clone, Debug)]
pub struct IdmTheme {
    pub dark: bool,
    pub bg_color: Color32,
    pub panel_bg: Color32,
    pub sidebar_bg: Color32,
    pub table_header_bg: Color32,
    pub table_row_alt: Color32,
    pub selection_bg: Color32,
    pub progress_fill: Color32,
    pub progress_bg: Color32,
    pub segment_bar_bg: Color32,
    pub segment_active: Color32,
    pub segment_done: Color32,
    pub segment_pending: Color32,
    pub text_primary: Color32,
    pub text_secondary: Color32,
    pub border_color: Color32,
    pub toolbar_bg: Color32,
    /// Slider rails, check boxes and text fields.
    pub control_bg: Color32,
    /// Button faces.
    pub button_bg: Color32,
    /// Menu bar strip and the line under it.
    pub menu_bg: Color32,
    pub menu_line: Color32,
    /// The downloads list.
    pub list_bg: Color32,
    pub grid_line: Color32,
    pub header_sep: Color32,
    /// "Categories" title strip of the tree.
    pub tree_header_bg: Color32,
    pub tree_selection: Color32,
    pub tree_selection_border: Color32,
    pub tree_lines: Color32,
    /// Text of a selected list row.
    pub selection_text: Color32,
    /// Toolbar line icons: enabled and disabled.
    pub icon_fg: Color32,
    pub icon_disabled: Color32,
    /// Inside of a tab page and the frame of a group box.
    pub page_bg: Color32,
    pub group_border: Color32,
}

impl Default for IdmTheme {
    fn default() -> Self {
        Self::idm_light()
    }
}

impl IdmTheme {
    pub fn idm_light() -> Self {
        Self {
            dark: false,
            bg_color: Color32::from_rgb(240, 240, 240),
            panel_bg: Color32::from_rgb(240, 240, 240),
            sidebar_bg: Color32::from_rgb(255, 255, 255),
            table_header_bg: Color32::from_rgb(255, 255, 255),
            table_row_alt: Color32::from_rgb(255, 255, 255),
            selection_bg: Color32::from_rgb(204, 232, 255),
            progress_fill: Color32::from_rgb(6, 176, 37),
            progress_bg: Color32::from_rgb(230, 230, 230),
            segment_bar_bg: Color32::from_rgb(230, 230, 230),
            segment_active: Color32::from_rgb(0, 120, 215),
            segment_done: Color32::from_rgb(6, 176, 37),
            segment_pending: Color32::from_rgb(255, 193, 7),
            text_primary: Color32::from_rgb(0, 0, 0),
            text_secondary: Color32::from_rgb(96, 96, 96),
            border_color: Color32::from_rgb(171, 173, 179),
            toolbar_bg: Color32::from_rgb(255, 255, 255),
            control_bg: Color32::from_rgb(214, 214, 214),
            button_bg: Color32::from_rgb(225, 225, 225),
            menu_bg: Color32::from_rgb(255, 255, 255),
            menu_line: Color32::from_rgb(160, 160, 160),
            list_bg: Color32::from_rgb(255, 255, 255),
            grid_line: Color32::from_rgb(240, 240, 240),
            header_sep: Color32::from_rgb(229, 229, 229),
            tree_header_bg: Color32::from_rgb(240, 240, 240),
            tree_selection: Color32::from_rgb(204, 232, 255),
            tree_selection_border: Color32::from_rgb(153, 209, 255),
            tree_lines: Color32::from_rgb(160, 160, 160),
            selection_text: Color32::from_rgb(0, 0, 0),
            icon_fg: Color32::from_rgb(96, 96, 96),
            icon_disabled: Color32::from_rgb(190, 190, 190),
            page_bg: Color32::from_rgb(249, 249, 249),
            group_border: Color32::from_rgb(220, 220, 220),
        }
    }

    /// The dark look with Windows in dark mode (colors measured from the reference).
    pub fn idm_dark() -> Self {
        Self {
            dark: true,
            bg_color: Color32::from_rgb(57, 57, 57),
            panel_bg: Color32::from_rgb(43, 43, 43),
            sidebar_bg: Color32::from_rgb(32, 32, 32),
            table_header_bg: Color32::from_rgb(25, 25, 25),
            table_row_alt: Color32::from_rgb(57, 57, 57),
            selection_bg: Color32::from_rgb(0, 84, 153),
            progress_fill: Color32::from_rgb(6, 176, 37),
            progress_bg: Color32::from_rgb(70, 70, 70),
            segment_bar_bg: Color32::from_rgb(70, 70, 70),
            segment_active: Color32::from_rgb(0, 120, 215),
            segment_done: Color32::from_rgb(6, 176, 37),
            segment_pending: Color32::from_rgb(241, 196, 15),
            text_primary: Color32::from_rgb(255, 255, 255),
            text_secondary: Color32::from_rgb(170, 170, 170),
            border_color: Color32::from_rgb(99, 99, 99),
            toolbar_bg: Color32::from_rgb(57, 57, 57),
            control_bg: Color32::from_rgb(80, 80, 80),
            button_bg: Color32::from_rgb(70, 70, 70),
            menu_bg: Color32::from_rgb(32, 32, 32),
            menu_line: Color32::from_rgb(160, 160, 160),
            list_bg: Color32::from_rgb(57, 57, 57),
            grid_line: Color32::from_rgb(85, 85, 85),
            header_sep: Color32::from_rgb(99, 99, 99),
            tree_header_bg: Color32::from_rgb(57, 57, 57),
            tree_selection: Color32::from_rgb(70, 70, 70),
            tree_selection_border: Color32::from_rgb(110, 110, 110),
            tree_lines: Color32::from_rgb(110, 110, 110),
            selection_text: Color32::from_rgb(255, 255, 255),
            icon_fg: Color32::from_rgb(200, 200, 200),
            icon_disabled: Color32::from_rgb(115, 115, 115),
            page_bg: Color32::from_rgb(50, 50, 50),
            group_border: Color32::from_rgb(85, 85, 85),
        }
    }

    pub fn apply(&self, ctx: &egui::Context) {
        // Keep egui on this look, and give the Windows title bar the same color:
        // Windows draws it dark when apps are dark, also for the light look.
        ctx.set_theme(if self.dark { egui::Theme::Dark } else { egui::Theme::Light });
        ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(if self.dark { egui::SystemTheme::Dark } else { egui::SystemTheme::Light }));
        let mut visuals = if self.dark { egui::Visuals::dark() } else { egui::Visuals::light() };

        visuals.window_fill = self.panel_bg;
        visuals.panel_fill = self.bg_color;
        visuals.extreme_bg_color = if self.dark { Color32::from_rgb(32, 32, 32) } else { Color32::WHITE };
        visuals.faint_bg_color = self.table_row_alt;
        visuals.window_rounding = Rounding::same(2.0);
        visuals.menu_rounding = Rounding::same(0.0);
        visuals.window_stroke = Stroke::new(1.0_f32, self.border_color);
        visuals.override_text_color = Some(self.text_primary);
        visuals.selection.bg_fill = self.selection_bg;
        visuals.selection.stroke = Stroke::new(1.0_f32, if self.dark { Color32::WHITE } else { Color32::from_rgb(0, 84, 153) });
        visuals.widgets.noninteractive.bg_fill = self.panel_bg;
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, self.border_color);
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, self.text_primary);
        // The slider rail uses bg_fill; it must stand out from the panel.
        visuals.widgets.inactive.bg_fill = self.control_bg;
        visuals.widgets.inactive.weak_bg_fill = self.button_bg;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, self.border_color);
        visuals.widgets.inactive.rounding = Rounding::same(2.0);
        visuals.widgets.hovered.bg_fill = if self.dark { Color32::from_rgb(77, 77, 77) } else { Color32::from_rgb(229, 241, 251) };
        visuals.widgets.hovered.weak_bg_fill = visuals.widgets.hovered.bg_fill;
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, if self.dark { Color32::from_rgb(120, 120, 120) } else { Color32::from_rgb(0, 120, 215) });
        visuals.widgets.hovered.rounding = Rounding::same(2.0);
        visuals.widgets.active.bg_fill = if self.dark { Color32::from_rgb(90, 90, 90) } else { Color32::from_rgb(204, 228, 247) };
        visuals.widgets.active.weak_bg_fill = visuals.widgets.active.bg_fill;
        visuals.widgets.active.rounding = Rounding::same(2.0);
        visuals.widgets.open.weak_bg_fill = visuals.widgets.hovered.bg_fill;

        ctx.set_visuals(visuals);

        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = Vec2::new(6.0, 4.0);
        // The metrics: 14 dialog unit buttons and boxes (22 px), 10 unit check boxes (16 px).
        style.spacing.button_padding = Vec2::new(8.0, 4.5);
        style.spacing.interact_size.y = 16.0;
        style.spacing.icon_width = 13.0;
        style.spacing.icon_width_inner = 7.0;
        style.spacing.menu_margin = egui::Margin::same(3.0);
        // The windows are drawn in Tahoma 8 pt; window titles are Segoe UI 9 pt.
        style.text_styles = [
            (TextStyle::Heading, menu_font()),
            (TextStyle::Body, FontId::proportional(LIST_FONT)),
            (TextStyle::Button, FontId::proportional(LIST_FONT)),
            (TextStyle::Small, FontId::proportional(9.3)),
            (TextStyle::Monospace, FontId::monospace(LIST_FONT)),
        ]
        .into();
        ctx.set_style(style);
        ctx.data_mut(|d| d.insert_temp(theme_id(), self.clone()));
    }
}

fn theme_id() -> egui::Id {
    egui::Id::new("klippo_theme")
}

/// The theme last applied with [`IdmTheme::apply`].
pub fn current(ctx: &egui::Context) -> IdmTheme {
    ctx.data(|d| d.get_temp::<IdmTheme>(theme_id())).unwrap_or_default()
}

/// Windows' own UI font (Segoe UI) first, egui's fonts after it for symbols.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".to_string());
    let read = |file: &str| std::fs::read(std::path::Path::new(&windir).join("Fonts").join(file)).ok();
    let add = |fonts: &mut FontDefinitions, name: &str, file: &str| -> bool {
        match read(file) {
            Some(bytes) => {
                fonts.font_data.insert(name.to_string(), FontData::from_owned(bytes).into());
                true
            }
            None => false,
        }
    };
    // Tahoma for the windows (the classic font), then Segoe UI for the characters Tahoma lacks.
    let mut proportional = Vec::new();
    for (name, file) in [("tahoma", "tahoma.ttf"), ("segoe_ui", "segoeui.ttf")] {
        if add(&mut fonts, name, file) {
            proportional.push(name.to_string());
        }
    }
    let defaults = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if add(&mut fonts, "segoe_symbol", "seguisym.ttf") {
        proportional.push("segoe_symbol".to_string());
    }
    proportional.extend(defaults);
    // Menus and window titles: Segoe UI, as Windows draws them.
    let mut menu: Vec<String> = proportional.iter().filter(|n| *n != "tahoma").cloned().collect();
    menu.extend(proportional.iter().filter(|n| *n == "tahoma").cloned());
    fonts.families.insert(FontFamily::Proportional, proportional);
    fonts.families.insert(FontFamily::Name("menu".into()), menu);
    ctx.set_fonts(fonts);
}

/// Segoe UI 9 pt: Windows menus and window titles.
pub fn menu_font() -> FontId {
    FontId::new(12.0, FontFamily::Name("menu".into()))
}

/// Menus use the Windows menu font instead of the dialog font.
pub fn menu_style(ui: &mut egui::Ui) {
    let font = menu_font();
    let styles = &mut ui.style_mut().text_styles;
    styles.insert(TextStyle::Button, font.clone());
    styles.insert(TextStyle::Body, font);
}
