use eframe::egui::{self, pos2, Align2, FontId, PopupCloseBehavior, Rect, Sense, Stroke, Ui, Vec2};

use crate::gui::icons::{self, SmallIcon};
use crate::gui::state::{Category, CategoryFilter, KlippoApp};
use crate::gui::theme;
use crate::gui::views::category_dialog::CategoryFrom;
use crate::gui::views::options_modal::OptionsTab;

/// The right-click menu of All Downloads and the categories.
enum TreeAction {
    Add,
    Delete(Category),
    Properties(Category),
}

const ROW: f32 = 15.2;
const INDENT: f32 = 15.2;

/// One line of the tree.
struct Node {
    level: usize,
    /// Key of a node with children (for folding), None for a leaf.
    fold: Option<&'static str>,
    icon: SmallIcon,
    label: String,
    filter: CategoryFilter,
    /// Last child of its parent (the guide line stops here).
    last: bool,
}

/// The "Categories" tree: All Downloads with its categories, Unfinished and
/// Finished with theirs, and the queues.
pub fn render_category_sidebar(app: &mut KlippoApp, ui: &mut Ui) {
    let theme = theme::current(ui.ctx());

    // Title strip with the close box.
    let (strip, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 15.0), Sense::hover());
    ui.painter().rect_filled(strip, 0.0, theme.tree_header_bg);
    ui.painter().text(strip.left_center() + Vec2::new(4.0, 0.0), Align2::LEFT_CENTER, "Categories", FontId::proportional(theme::LIST_FONT), theme.text_primary);
    let close = Rect::from_center_size(strip.right_center() - Vec2::new(9.0, 0.0), Vec2::splat(13.0));
    let close_resp = ui.interact(close, ui.id().with("categories_close"), Sense::click()).on_hover_text("Hide categories");
    if close_resp.hovered() {
        ui.painter().rect_filled(close, 2.0, ui.visuals().widgets.hovered.bg_fill);
    }
    let x = Stroke::new(1.2_f32, theme.text_primary);
    ui.painter().line_segment([close.center() + Vec2::new(-3.2, -3.2), close.center() + Vec2::new(3.2, 3.2)], x);
    ui.painter().line_segment([close.center() + Vec2::new(3.2, -3.2), close.center() + Vec2::new(-3.2, 3.2)], x);
    if close_resp.clicked() {
        app.show_sidebar = false;
    }
    ui.add_space(2.0);

    // General is "All Downloads"; the user's categories come after the fixed ones.
    let categories: Vec<Category> = Category::all().into_iter().skip(1).collect();
    let mut nodes = Vec::new();
    let branch = |nodes: &mut Vec<Node>, key: &'static str, icon: SmallIcon, label: &str, filter: CategoryFilter, children: Vec<(SmallIcon, String, CategoryFilter)>, open: bool| {
        nodes.push(Node { level: 0, fold: Some(key), icon, label: label.to_string(), filter, last: false });
        if open {
            let n = children.len();
            for (i, (icon, label, filter)) in children.into_iter().enumerate() {
                nodes.push(Node { level: 1, fold: None, icon, label, filter, last: i + 1 == n });
            }
        }
    };
    let open = |key: &str| !app.tree_closed.contains(key);
    let all_open = open("all");
    branch(
        &mut nodes,
        "all",
        if all_open { SmallIcon::FolderOpen } else { SmallIcon::Folder },
        "All Downloads",
        CategoryFilter::All,
        categories.iter().map(|c| (SmallIcon::Category(*c), c.name().to_string(), CategoryFilter::Category(*c))).collect(),
        all_open,
    );
    branch(
        &mut nodes,
        "unfinished",
        SmallIcon::Unfinished,
        "Unfinished",
        CategoryFilter::Unfinished,
        categories.iter().map(|c| (SmallIcon::Category(*c), c.name().to_string(), CategoryFilter::UnfinishedIn(*c))).collect(),
        open("unfinished"),
    );
    branch(
        &mut nodes,
        "finished",
        SmallIcon::Finished,
        "Finished",
        CategoryFilter::Finished,
        categories.iter().map(|c| (SmallIcon::Category(*c), c.name().to_string(), CategoryFilter::FinishedIn(*c))).collect(),
        open("finished"),
    );
    let queues: Vec<(SmallIcon, String, CategoryFilter)> =
        app.settings.queues.iter().map(|q| (SmallIcon::Queue, q.name.clone(), CategoryFilter::Queue(q.id))).collect();
    let first_queue = queues.first().map(|q| q.2).unwrap_or(CategoryFilter::All);
    branch(&mut nodes, "queues", SmallIcon::Queues, "Queues", first_queue, queues, open("queues"));

    let mut toggle: Option<&'static str> = None;
    let mut pick: Option<CategoryFilter> = None;
    let mut action: Option<TreeAction> = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let left = ui.max_rect().left() + 2.0;
        let mut parent_y = 0.0;
        for (i, node) in nodes.iter().enumerate() {
            let (row, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW), Sense::hover());
            let p = ui.painter();
            let cy = row.center().y;
            let icon_x = left + 13.0 + node.level as f32 * INDENT;
            if node.level == 0 {
                parent_y = cy;
                if let Some(key) = node.fold {
                    let hit = Rect::from_center_size(pos2(left + 5.0, cy), Vec2::splat(12.0));
                    let resp = ui.interact(hit, ui.id().with(("tree_fold", key)), Sense::click());
                    let border = if resp.hovered() { ui.visuals().widgets.hovered.bg_stroke.color } else { theme.tree_lines };
                    icons::plus_box(p, hit.center(), open(key), border, theme.text_primary);
                    if resp.clicked() {
                        toggle = Some(key);
                    }
                }
            } else {
                // Guide lines from the parent's icon down to this child.
                let gx = left + 13.0 + 6.4;
                icons::dotted_vline(p, gx.round(), parent_y + 7.0, if node.last { cy } else { row.bottom() }, theme.tree_lines);
                icons::dotted_hline(p, gx.round() + 2.0, icon_x - 1.0, cy.round(), theme.tree_lines);
                parent_y = row.bottom() - 7.6;
            }
            let icon_rect = Rect::from_min_size(pos2(icon_x, cy - 6.4), Vec2::splat(12.8));
            let font = FontId::proportional(theme::LIST_FONT);
            let text_w = ui.fonts(|f| f.layout_no_wrap(node.label.clone(), font.clone(), theme.text_primary).size().x);
            let label_rect = Rect::from_min_max(pos2(icon_rect.left() - 2.0, row.top() + 1.0), pos2(icon_rect.right() + 6.0 + text_w + 4.0, row.bottom() - 1.0));
            let resp = ui.interact(label_rect, ui.id().with(("tree_node", i)), Sense::click()).on_hover_text(&node.label);
            let selected = app.selected_category == node.filter && !(node.fold == Some("queues"));
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &node.label));
            if selected {
                p.rect_filled(label_rect, 0.0, theme.tree_selection);
                p.rect_stroke(label_rect, 0.0, Stroke::new(1.0_f32, theme.tree_selection_border));
            } else if resp.hovered() {
                p.rect_filled(label_rect, 0.0, ui.visuals().widgets.hovered.bg_fill);
            }
            icons::small(p, icon_rect, node.icon);
            p.text(pos2(icon_rect.right() + 4.0, cy), Align2::LEFT_CENTER, &node.label, font, if selected { theme.selection_text } else { theme.text_primary });
            if resp.clicked() {
                if node.fold == Some("queues") {
                    toggle = Some("queues");
                } else {
                    pick = Some(node.filter);
                }
            }
            if resp.double_clicked() {
                toggle = node.fold;
            }
            let menu_for = match node.filter {
                CategoryFilter::Category(c) | CategoryFilter::UnfinishedIn(c) | CategoryFilter::FinishedIn(c) => Some(Some(c)),
                CategoryFilter::All => Some(None),
                _ => None,
            };
            if let Some(cat) = menu_for {
                // A right-click, or Shift+F10 on the item that has the keyboard (as in Windows).
                let popup = ui.make_persistent_id(("tree_menu", i));
                let shift_f10 = resp.has_focus() && ui.input(|inp| inp.modifiers.shift && inp.key_pressed(egui::Key::F10));
                if resp.secondary_clicked() || shift_f10 {
                    ui.memory_mut(|m| m.open_popup(popup));
                }
                egui::popup_below_widget(ui, popup, &resp, PopupCloseBehavior::CloseOnClickOutside, |ui| {
                    theme::menu_style(ui);
                    ui.set_min_width(150.0);
                    if ui.button("Add category...").clicked() {
                        action = Some(TreeAction::Add);
                    }
                    if let Some(c) = cat {
                        if ui.add_enabled(c.is_user(), egui::Button::new("Delete category")).clicked() {
                            action = Some(TreeAction::Delete(c));
                        }
                        if ui.button("Properties...").clicked() {
                            action = Some(TreeAction::Properties(c));
                        }
                    }
                });
            }
        }
    });
    if action.is_some() {
        ui.memory_mut(|m| m.close_popup());
    }
    match action {
        Some(TreeAction::Add) => app.open_new_category(CategoryFrom::Tree),
        Some(TreeAction::Delete(c)) => {
            let name = c.name();
            app.delete_category(c);
            app.set_status(format!("Deleted the category \"{}\"", name));
        }
        Some(TreeAction::Properties(c)) => {
            // Options > Save to shows the category's file types and folder.
            app.open_options();
            app.options_active_tab = OptionsTab::SaveTo;
            app.options_save_to_category = c;
        }
        None => {}
    }
    if let Some(key) = toggle {
        if !app.tree_closed.remove(key) {
            app.tree_closed.insert(key);
        }
    }
    if let Some(filter) = pick {
        app.selected_category = filter;
    }
}
