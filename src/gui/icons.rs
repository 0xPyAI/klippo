//! Icons drawn with the painter, in the style of a classic download manager: thin line icons
//! with a color accent on the toolbar, small filled icons in the tree and the
//! list. They are Klippo's own drawings (no copied bitmaps).

use eframe::egui::{epaint::PathShape, pos2, Color32, Painter, Pos2, Rect, Rounding, Shape, Stroke, Vec2};

use crate::gui::state::Category;
use crate::gui::theme::IdmTheme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolIcon {
    AddUrl,
    Resume,
    Stop,
    StopAll,
    Delete,
    DeleteCompleted,
    Options,
    Scheduler,
    StartQueue,
    StopQueue,
    Grabber,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmallIcon {
    FolderOpen,
    Folder,
    Category(Category),
    Unfinished,
    Finished,
    Queues,
    Queue,
}

/// Maps a point of the unit square onto `r`.
fn at(r: Rect, x: f32, y: f32) -> Pos2 {
    pos2(r.left() + x * r.width(), r.top() + y * r.height())
}

fn polyline(p: &Painter, r: Rect, pts: &[(f32, f32)], closed: bool, stroke: Stroke) {
    let points = pts.iter().map(|&(x, y)| at(r, x, y)).collect();
    p.add(if closed { PathShape::closed_line(points, stroke) } else { PathShape::line(points, stroke) });
}

fn filled(p: &Painter, r: Rect, pts: &[(f32, f32)], fill: Color32, stroke: Stroke) {
    let points = pts.iter().map(|&(x, y)| at(r, x, y)).collect();
    p.add(Shape::convex_polygon(points, fill, stroke));
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Two-tone colors of the toolbar icons, in classic hues (every enabled icon
/// is colored; disabled ones are grey).
fn accent(icon: ToolIcon, theme: &IdmTheme) -> (Color32, Color32) {
    let rgb = Color32::from_rgb;
    let (a, b) = match icon {
        ToolIcon::AddUrl => (rgb(64, 182, 150), rgb(110, 205, 175)),
        ToolIcon::Resume => (rgb(60, 150, 230), rgb(90, 200, 230)),
        ToolIcon::Stop => (rgb(150, 100, 210), rgb(225, 95, 150)),
        ToolIcon::StopAll => (rgb(230, 85, 110), rgb(240, 140, 120)),
        ToolIcon::Delete => (rgb(240, 85, 100), rgb(240, 135, 110)),
        ToolIcon::DeleteCompleted => (rgb(200, 80, 160), rgb(130, 95, 215)),
        ToolIcon::Options => (rgb(110, 110, 235), rgb(150, 120, 240)),
        ToolIcon::Scheduler => (rgb(238, 160, 90), rgb(205, 125, 70)),
        ToolIcon::StartQueue => (rgb(70, 150, 150), rgb(80, 170, 225)),
        ToolIcon::StopQueue => (rgb(110, 90, 225), rgb(225, 70, 140)),
        ToolIcon::Grabber => (rgb(150, 100, 225), rgb(240, 150, 80)),
    };
    if theme.dark {
        (mix(a, Color32::WHITE, 0.12), mix(b, Color32::WHITE, 0.12))
    } else {
        (a, b)
    }
}

/// A toolbar icon in `r` (about 30 x 30 points).
pub fn tool(p: &Painter, r: Rect, icon: ToolIcon, enabled: bool, theme: &IdmTheme) {
    let (main, second) = if enabled { accent(icon, theme) } else { (theme.icon_disabled, theme.icon_disabled) };
    let w = (r.width() / 15.0).max(1.4);
    let s = Stroke::new(w, main);
    let s2 = Stroke::new(w, second);
    let bg = theme.toolbar_bg;
    match icon {
        ToolIcon::AddUrl => {
            let (a, b) = (0.38, 0.62);
            polyline(
                p,
                r,
                &[(a, 0.1), (b, 0.1), (b, a), (0.9, a), (0.9, b), (b, b), (b, 0.9), (a, 0.9), (a, b), (0.1, b), (0.1, a), (a, a)],
                true,
                s,
            );
        }
        ToolIcon::Resume => {
            p.circle_stroke(at(r, 0.5, 0.5), r.width() * 0.42, s);
            polyline(p, r, &[(0.5, 0.27), (0.5, 0.7)], false, s);
            polyline(p, r, &[(0.34, 0.54), (0.5, 0.7), (0.66, 0.54)], false, s);
        }
        ToolIcon::Stop => {
            p.circle_stroke(at(r, 0.5, 0.5), r.width() * 0.42, s);
            p.rect_stroke(Rect::from_min_max(at(r, 0.36, 0.36), at(r, 0.64, 0.64)), Rounding::same(2.0), s);
        }
        ToolIcon::StopAll => {
            // An open hand: four fingers, a thumb and the palm.
            for (i, x) in [0.3, 0.44, 0.58, 0.72].iter().enumerate() {
                let top = [0.2, 0.1, 0.13, 0.22][i];
                let rect = Rect::from_min_max(at(r, x - 0.055, top), at(r, x + 0.055, 0.6));
                p.rect_filled(rect, Rounding::same(r.width() * 0.055), bg);
                p.rect_stroke(rect, Rounding::same(r.width() * 0.055), s);
            }
            polyline(p, r, &[(0.245, 0.55), (0.25, 0.72), (0.36, 0.9), (0.68, 0.9), (0.775, 0.72), (0.775, 0.55)], false, s);
            polyline(p, r, &[(0.245, 0.6), (0.15, 0.48), (0.08, 0.52), (0.2, 0.75)], false, s);
        }
        ToolIcon::Delete | ToolIcon::DeleteCompleted => {
            polyline(p, r, &[(0.18, 0.24), (0.82, 0.24)], false, s);
            polyline(p, r, &[(0.38, 0.24), (0.4, 0.12), (0.6, 0.12), (0.62, 0.24)], false, s);
            polyline(p, r, &[(0.25, 0.3), (0.31, 0.9), (0.69, 0.9), (0.75, 0.3)], false, s);
            if icon == ToolIcon::Delete {
                polyline(p, r, &[(0.42, 0.42), (0.43, 0.78)], false, s);
                polyline(p, r, &[(0.58, 0.42), (0.57, 0.78)], false, s);
            } else {
                polyline(p, r, &[(0.36, 0.58), (0.47, 0.7), (0.66, 0.45)], false, s2);
            }
        }
        ToolIcon::Options => {
            // A gear: eight teeth around a ring.
            let c = at(r, 0.5, 0.5);
            let (outer, inner) = (r.width() * 0.45, r.width() * 0.33);
            let mut pts = Vec::new();
            for i in 0..8 {
                let base = i as f32 * std::f32::consts::TAU / 8.0;
                for (da, rad) in [(-0.2, inner), (-0.12, outer), (0.12, outer), (0.2, inner)] {
                    let a = base + da;
                    pts.push(c + Vec2::new(a.cos(), a.sin()) * rad);
                }
            }
            p.add(PathShape::closed_line(pts, s));
            p.circle_stroke(c, r.width() * 0.14, s2);
        }
        ToolIcon::Scheduler => {
            let c = at(r, 0.5, 0.56);
            p.circle_stroke(c, r.width() * 0.34, s);
            polyline(p, r, &[(0.14, 0.26), (0.24, 0.14), (0.34, 0.12)], false, s);
            polyline(p, r, &[(0.86, 0.26), (0.76, 0.14), (0.66, 0.12)], false, s);
            polyline(p, r, &[(0.5, 0.36), (0.5, 0.56), (0.63, 0.65)], false, s2);
            polyline(p, r, &[(0.3, 0.84), (0.22, 0.93)], false, s);
            polyline(p, r, &[(0.7, 0.84), (0.78, 0.93)], false, s);
        }
        ToolIcon::StartQueue | ToolIcon::StopQueue => {
            let back = Rect::from_min_max(at(r, 0.32, 0.1), at(r, 0.9, 0.66));
            let front = Rect::from_min_max(at(r, 0.1, 0.32), at(r, 0.68, 0.9));
            let round = Rounding::same(r.width() * 0.08);
            p.rect_stroke(back, round, s);
            p.rect_filled(front.expand(w), round, bg);
            p.rect_stroke(front, round, s);
            if icon == ToolIcon::StartQueue {
                polyline(p, r, &[(0.39, 0.45), (0.39, 0.76)], false, s2);
                polyline(p, r, &[(0.28, 0.65), (0.39, 0.76), (0.5, 0.65)], false, s2);
            } else {
                p.rect_stroke(Rect::from_min_max(at(r, 0.28, 0.5), at(r, 0.5, 0.72)), Rounding::same(1.5), s2);
            }
        }
        ToolIcon::Grabber => {
            p.rect_stroke(Rect::from_min_max(at(r, 0.1, 0.14), at(r, 0.9, 0.7)), Rounding::same(r.width() * 0.06), s);
            filled(p, r, &[(0.43, 0.3), (0.43, 0.54), (0.6, 0.42)], main, Stroke::NONE);
            polyline(p, r, &[(0.2, 0.86), (0.8, 0.86)], false, s2);
            polyline(p, r, &[(0.5, 0.7), (0.5, 0.86)], false, s2);
        }
    }
}

/// Classic yellow folder (closed or open).
fn folder(p: &Painter, r: Rect, open: bool, body: Color32) {
    let edge = Stroke::new(1.0_f32, mix(body, Color32::BLACK, 0.35));
    filled(p, r, &[(0.06, 0.2), (0.4, 0.2), (0.48, 0.3), (0.94, 0.3), (0.94, 0.86), (0.06, 0.86)], mix(body, Color32::BLACK, 0.12), edge);
    if open {
        filled(p, r, &[(0.2, 0.44), (1.0, 0.44), (0.86, 0.86), (0.06, 0.86)], mix(body, Color32::WHITE, 0.25), edge);
    } else {
        p.rect_filled(Rect::from_min_max(at(r, 0.06, 0.38), at(r, 0.94, 0.86)), Rounding::ZERO, body);
        p.rect_stroke(Rect::from_min_max(at(r, 0.06, 0.38), at(r, 0.94, 0.86)), Rounding::ZERO, edge);
    }
}

fn doc_page(p: &Painter, r: Rect) {
    let edge = Stroke::new(1.0_f32, Color32::from_rgb(130, 130, 130));
    filled(p, r, &[(0.2, 0.06), (0.62, 0.06), (0.82, 0.26), (0.82, 0.94), (0.2, 0.94)], Color32::from_rgb(250, 250, 250), edge);
    polyline(p, r, &[(0.62, 0.06), (0.62, 0.26), (0.82, 0.26)], false, edge);
    for y in [0.42, 0.56, 0.7, 0.84] {
        polyline(p, r, &[(0.3, y), (0.72, y)], false, Stroke::new(1.0_f32, Color32::from_rgb(120, 150, 210)));
    }
}

/// A 16 x 16 icon for the category tree and the file list.
pub fn small(p: &Painter, r: Rect, icon: SmallIcon) {
    let yellow = Color32::from_rgb(246, 202, 84);
    match icon {
        SmallIcon::FolderOpen => folder(p, r, true, yellow),
        SmallIcon::Folder => folder(p, r, false, yellow),
        SmallIcon::Unfinished => {
            folder(p, r, false, Color32::from_rgb(118, 196, 96));
            let arrow = Stroke::new(1.6_f32, Color32::from_rgb(20, 110, 40));
            polyline(p, r, &[(0.5, 0.42), (0.5, 0.78)], false, arrow);
            polyline(p, r, &[(0.36, 0.64), (0.5, 0.78), (0.64, 0.64)], false, arrow);
        }
        SmallIcon::Finished => {
            folder(p, r, false, Color32::from_rgb(120, 170, 235));
            polyline(p, r, &[(0.3, 0.6), (0.45, 0.75), (0.74, 0.46)], false, Stroke::new(1.8_f32, Color32::from_rgb(255, 255, 255)));
        }
        SmallIcon::Queues => {
            folder(p, Rect::from_min_size(at(r, 0.16, 0.0), r.size() * 0.84), false, mix(yellow, Color32::WHITE, 0.2));
            folder(p, Rect::from_min_size(at(r, 0.0, 0.16), r.size() * 0.84), false, yellow);
        }
        SmallIcon::Queue => {
            let edge = Stroke::new(1.0_f32, Color32::from_rgb(70, 110, 190));
            p.rect_filled(Rect::from_min_max(at(r, 0.3, 0.1), at(r, 0.92, 0.66)), Rounding::same(1.0), Color32::from_rgb(200, 220, 250));
            p.rect_stroke(Rect::from_min_max(at(r, 0.3, 0.1), at(r, 0.92, 0.66)), Rounding::same(1.0), edge);
            p.rect_filled(Rect::from_min_max(at(r, 0.08, 0.34), at(r, 0.7, 0.9)), Rounding::same(1.0), Color32::from_rgb(120, 160, 230));
            p.rect_stroke(Rect::from_min_max(at(r, 0.08, 0.34), at(r, 0.7, 0.9)), Rounding::same(1.0), edge);
        }
        SmallIcon::Category(cat) => match cat {
            Category::Compressed => {
                let edge = Stroke::new(1.0_f32, Color32::from_rgb(150, 110, 20));
                p.rect_filled(Rect::from_min_max(at(r, 0.16, 0.06), at(r, 0.84, 0.94)), Rounding::same(1.0), yellow);
                p.rect_stroke(Rect::from_min_max(at(r, 0.16, 0.06), at(r, 0.84, 0.94)), Rounding::same(1.0), edge);
                for (i, y) in [0.14, 0.26, 0.38, 0.5].iter().enumerate() {
                    let x = if i % 2 == 0 { 0.42 } else { 0.5 };
                    p.rect_filled(Rect::from_min_max(at(r, x, *y), at(r, x + 0.1, y + 0.08)), Rounding::ZERO, Color32::from_rgb(110, 80, 20));
                }
                p.rect_stroke(Rect::from_min_max(at(r, 0.4, 0.62), at(r, 0.6, 0.84)), Rounding::ZERO, Stroke::new(1.0_f32, Color32::from_rgb(110, 80, 20)));
            }
            Category::Documents | Category::General => doc_page(p, r),
            Category::User(_) => folder(p, r, false, yellow),
            Category::Music => {
                let blue = Color32::from_rgb(52, 132, 228);
                p.circle_filled(at(r, 0.3, 0.78), r.width() * 0.16, blue);
                p.circle_filled(at(r, 0.74, 0.68), r.width() * 0.16, blue);
                let s = Stroke::new(r.width() * 0.1, blue);
                polyline(p, r, &[(0.42, 0.78), (0.42, 0.18)], false, s);
                polyline(p, r, &[(0.86, 0.68), (0.86, 0.08)], false, s);
                filled(p, r, &[(0.42, 0.14), (0.9, 0.04), (0.9, 0.2), (0.42, 0.3)], blue, Stroke::NONE);
            }
            Category::Programs => {
                let edge = Stroke::new(1.0_f32, Color32::from_rgb(60, 80, 140));
                p.rect_filled(Rect::from_min_max(at(r, 0.06, 0.16), at(r, 0.94, 0.86)), Rounding::same(1.0), Color32::from_rgb(245, 245, 250));
                p.rect_filled(Rect::from_min_max(at(r, 0.06, 0.16), at(r, 0.94, 0.36)), Rounding::ZERO, Color32::from_rgb(80, 110, 210));
                p.rect_stroke(Rect::from_min_max(at(r, 0.06, 0.16), at(r, 0.94, 0.86)), Rounding::same(1.0), edge);
            }
            Category::Video => {
                let dark = Color32::from_rgb(70, 76, 90);
                p.rect_filled(Rect::from_min_max(at(r, 0.06, 0.14), at(r, 0.94, 0.86)), Rounding::same(1.0), dark);
                p.rect_filled(Rect::from_min_max(at(r, 0.24, 0.24), at(r, 0.76, 0.76)), Rounding::ZERO, Color32::from_rgb(110, 190, 245));
                for y in [0.2, 0.42, 0.64] {
                    p.rect_filled(Rect::from_min_max(at(r, 0.1, y), at(r, 0.18, y + 0.12)), Rounding::ZERO, Color32::WHITE);
                    p.rect_filled(Rect::from_min_max(at(r, 0.82, y), at(r, 0.9, y + 0.12)), Rounding::ZERO, Color32::WHITE);
                }
            }
        },
    }
}

/// A small chevron for tree nodes: pointing down when `open`.
pub fn chevron(p: &Painter, center: Pos2, open: bool, color: Color32) {
    let d = 3.5;
    let pts = if open {
        vec![center + Vec2::new(-d, -d / 2.0), center + Vec2::new(0.0, d / 2.0), center + Vec2::new(d, -d / 2.0)]
    } else {
        vec![center + Vec2::new(-d / 2.0, -d), center + Vec2::new(d / 2.0, 0.0), center + Vec2::new(-d / 2.0, d)]
    };
    p.add(PathShape::line(pts, Stroke::new(1.2_f32, color)));
}

/// The [+] / [-] box of a Windows tree view, as the Categories tree has.
pub fn plus_box(p: &Painter, center: Pos2, open: bool, border: Color32, sign: Color32) {
    let r = Rect::from_center_size(center, Vec2::splat(7.2));
    p.rect_stroke(r, Rounding::ZERO, Stroke::new(0.8_f32, border));
    let s = Stroke::new(0.8_f32, sign);
    p.hline(r.left() + 1.6..=r.right() - 1.6, center.y, s);
    if !open {
        p.vline(center.x, r.top() + 1.6..=r.bottom() - 1.6, s);
    }
}

/// The dotted guide lines of a Windows tree view.
pub fn dotted_hline(p: &Painter, x1: f32, x2: f32, y: f32, color: Color32) {
    let mut x = x1;
    while x < x2 {
        p.rect_filled(Rect::from_min_size(pos2(x, y), Vec2::splat(1.0)), Rounding::ZERO, color);
        x += 2.0;
    }
}

pub fn dotted_vline(p: &Painter, x: f32, y1: f32, y2: f32, color: Color32) {
    let mut y = y1;
    while y < y2 {
        p.rect_filled(Rect::from_min_size(pos2(x, y), Vec2::splat(1.0)), Rounding::ZERO, color);
        y += 2.0;
    }
}

/// The 32-point picture at the top-left of an Options page or dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageIcon {
    General,
    FileTypes,
    SaveTo,
    Downloads,
    Connection,
    Proxy,
    SiteLogins,
    DialUp,
    Sounds,
    Scheduler,
}

fn globe(p: &Painter, r: Rect) {
    let c = r.center();
    let rad = r.width() / 2.0;
    p.circle_filled(c, rad, Color32::from_rgb(52, 120, 214));
    p.circle_filled(c + Vec2::new(-rad * 0.25, -rad * 0.25), rad * 0.55, Color32::from_rgb(96, 168, 240));
    filled(p, r, &[(0.3, 0.25), (0.55, 0.3), (0.5, 0.55), (0.3, 0.5)], Color32::from_rgb(90, 180, 90), Stroke::NONE);
    filled(p, r, &[(0.6, 0.6), (0.8, 0.55), (0.75, 0.8), (0.6, 0.78)], Color32::from_rgb(90, 180, 90), Stroke::NONE);
    p.circle_stroke(c, rad, Stroke::new(1.0_f32, Color32::from_rgb(30, 80, 160)));
}

fn monitor(p: &Painter, r: Rect) {
    let edge = Stroke::new(1.0_f32, Color32::from_rgb(70, 80, 100));
    p.rect_filled(Rect::from_min_max(at(r, 0.05, 0.05), at(r, 0.95, 0.72)), Rounding::same(1.5), Color32::from_rgb(225, 230, 240));
    p.rect_filled(Rect::from_min_max(at(r, 0.12, 0.12), at(r, 0.88, 0.62)), Rounding::ZERO, Color32::from_rgb(70, 140, 220));
    p.rect_stroke(Rect::from_min_max(at(r, 0.05, 0.05), at(r, 0.95, 0.72)), Rounding::same(1.5), edge);
    filled(p, r, &[(0.38, 0.72), (0.62, 0.72), (0.7, 0.92), (0.3, 0.92)], Color32::from_rgb(150, 155, 170), Stroke::NONE);
}

fn sub(r: Rect, x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
    Rect::from_min_max(at(r, x0, y0), at(r, x1, y1))
}

pub fn page(p: &Painter, r: Rect, icon: PageIcon) {
    match icon {
        PageIcon::General => {
            monitor(p, sub(r, 0.0, 0.1, 0.72, 0.82));
            globe(p, sub(r, 0.45, 0.45, 1.0, 1.0));
        }
        PageIcon::FileTypes => {
            small(p, sub(r, 0.3, 0.0, 0.9, 0.55), SmallIcon::Category(Category::Programs));
            small(p, sub(r, 0.0, 0.3, 0.55, 0.85), SmallIcon::Folder);
            small(p, sub(r, 0.45, 0.45, 1.0, 1.0), SmallIcon::Category(Category::Programs));
        }
        PageIcon::SaveTo => {
            small(p, sub(r, 0.0, 0.05, 1.0, 1.0), SmallIcon::FolderOpen);
            page_sheet(p, sub(r, 0.35, 0.0, 0.85, 0.5));
        }
        PageIcon::Downloads => {
            globe(p, sub(r, 0.08, 0.08, 0.92, 0.92));
            let arrow = Stroke::new(r.width() * 0.09, Color32::from_rgb(40, 170, 70));
            polyline(p, r, &[(0.5, 0.25), (0.5, 0.75)], false, arrow);
            polyline(p, r, &[(0.3, 0.55), (0.5, 0.75), (0.7, 0.55)], false, arrow);
        }
        PageIcon::Connection => {
            monitor(p, sub(r, 0.0, 0.0, 0.6, 0.6));
            monitor(p, sub(r, 0.4, 0.4, 1.0, 1.0));
            globe(p, sub(r, 0.05, 0.55, 0.45, 0.95));
        }
        PageIcon::Proxy => {
            let edge = Stroke::new(1.0_f32, Color32::from_rgb(60, 70, 100));
            p.rect_filled(sub(r, 0.2, 0.04, 0.8, 0.96), Rounding::same(2.0), Color32::from_rgb(170, 185, 215));
            p.rect_stroke(sub(r, 0.2, 0.04, 0.8, 0.96), Rounding::same(2.0), edge);
            for y in [0.18, 0.32, 0.46] {
                p.rect_filled(sub(r, 0.3, y, 0.7, y + 0.06), Rounding::ZERO, Color32::from_rgb(70, 90, 140));
            }
            p.circle_filled(at(r, 0.5, 0.78), r.width() * 0.06, Color32::from_rgb(80, 200, 90));
        }
        PageIcon::SiteLogins => {
            let gold = Color32::from_rgb(230, 180, 40);
            p.circle_filled(at(r, 0.68, 0.28), r.width() * 0.2, gold);
            p.circle_filled(at(r, 0.68, 0.28), r.width() * 0.07, Color32::WHITE);
            polyline(p, r, &[(0.58, 0.42), (0.36, 0.9)], false, Stroke::new(r.width() * 0.1, gold));
            polyline(p, r, &[(0.42, 0.78), (0.5, 0.82)], false, Stroke::new(r.width() * 0.08, gold));
            p.circle_filled(at(r, 0.24, 0.42), r.width() * 0.13, Color32::from_rgb(70, 160, 80));
            filled(p, r, &[(0.04, 0.95), (0.08, 0.66), (0.4, 0.66), (0.44, 0.95)], Color32::from_rgb(70, 160, 80), Stroke::NONE);
        }
        PageIcon::DialUp => {
            monitor(p, sub(r, 0.35, 0.0, 1.0, 0.65));
            let yellow = Color32::from_rgb(235, 190, 60);
            p.rect_filled(sub(r, 0.0, 0.5, 0.62, 0.95), Rounding::same(3.0), yellow);
            p.rect_stroke(sub(r, 0.0, 0.5, 0.62, 0.95), Rounding::same(3.0), Stroke::new(1.0_f32, Color32::from_rgb(150, 110, 20)));
            for (x, y) in [(0.15, 0.68), (0.31, 0.68), (0.47, 0.68), (0.15, 0.82), (0.31, 0.82), (0.47, 0.82)] {
                p.circle_filled(at(r, x, y), r.width() * 0.035, Color32::from_rgb(110, 80, 20));
            }
        }
        PageIcon::Sounds => {
            small(p, sub(r, 0.0, 0.05, 0.62, 0.95), SmallIcon::Category(Category::Music));
            let grey = Color32::from_rgb(120, 125, 140);
            filled(p, r, &[(0.55, 0.42), (0.68, 0.42), (0.9, 0.2), (0.9, 0.95), (0.68, 0.72), (0.55, 0.72)], grey, Stroke::new(1.0_f32, Color32::from_rgb(70, 70, 80)));
        }
        PageIcon::Scheduler => {
            small(p, sub(r, 0.0, 0.25, 0.85, 1.0), SmallIcon::Folder);
            let face = sub(r, 0.32, 0.0, 1.0, 0.68);
            p.circle_filled(face.center(), face.width() * 0.42, Color32::from_rgb(250, 250, 245));
            p.circle_stroke(face.center(), face.width() * 0.42, Stroke::new(r.width() * 0.07, Color32::from_rgb(230, 120, 40)));
            polyline(p, face, &[(0.5, 0.28), (0.5, 0.5), (0.68, 0.6)], false, Stroke::new(1.4_f32, Color32::from_rgb(60, 60, 60)));
        }
    }
}

fn page_sheet(p: &Painter, r: Rect) {
    small(p, r, SmallIcon::Category(Category::Documents));
}
