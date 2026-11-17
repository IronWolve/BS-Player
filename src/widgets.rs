//! Compact, vector-drawn controls used by the player surface.

use eframe::egui::{
    self, Button, Color32, CornerRadius, FontId, Mesh, Painter, Pos2, Rect, Response, Sense, Shape,
    Stroke, StrokeKind, Ui, Vec2, WidgetInfo, WidgetType,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Play,
    Pause,
    Previous,
    Next,
    Stop,
    Shuffle,
    Repeat,
    Bookmark,
    Moon,
    Volume,
    Muted,
    Plus,
    Folder,
    Playlist,
    Music,
    Book,
    Clock,
    Continue,
    /// A check mark in a circle: finished.
    Check,
    Settings,
    Appearance,
    Equalizer,
    Tag,
    File,
    Keyboard,
    Bell,
    Backup,
    Expand,
    Search,
    /// A small radio set: a live stream.
    Radio,
    /// A head and shoulders: the Artists list.
    Artist,
    /// A disc: the Albums list.
    Album,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WindowControl {
    Minimize,
    Maximize,
    Restore,
    Close,
}

/// Loads the same BS Podcasts shell icon used for the Windows executable.
pub fn application_icon() -> egui::IconData {
    let image = image::load_from_memory(include_bytes!(
        "../assets/branding/bs-podcasts-shell-icon-512.png"
    ))
    .expect("embedded application icon is a valid PNG")
    .into_rgba8();
    let (width, height) = image.dimensions();
    egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    }
}

const ICON_STROKE: f32 = 1.6;

fn point(rect: Rect, x: f32, y: f32) -> Pos2 {
    let side = rect.width().min(rect.height());
    let origin = rect.center() - Vec2::splat(side * 0.5);
    origin + Vec2::new(x * side / 24.0, y * side / 24.0)
}

fn path(painter: &Painter, rect: Rect, coords: &[(f32, f32)], color: Color32, width: f32) {
    if let [a, b] = coords {
        painter.line_segment(
            [point(rect, a.0, a.1), point(rect, b.0, b.1)],
            Stroke::new(width, color),
        );
        return;
    }
    let points = coords
        .iter()
        .map(|&(x, y)| point(rect, x, y))
        .collect::<Vec<_>>();
    if points.len() >= 2 {
        painter.add(Shape::line(points, Stroke::new(width, color)));
    }
}

fn polygon(painter: &Painter, rect: Rect, coords: &[(f32, f32)], color: Color32) {
    let points = coords
        .iter()
        .map(|&(x, y)| point(rect, x, y))
        .collect::<Vec<_>>();
    if points.len() >= 3 {
        painter.add(Shape::convex_polygon(
            points,
            color,
            Stroke::new(ICON_STROKE, color),
        ));
    }
}

fn line(painter: &Painter, rect: Rect, a: (f32, f32), b: (f32, f32), color: Color32) {
    painter.line_segment(
        [point(rect, a.0, a.1), point(rect, b.0, b.1)],
        Stroke::new(ICON_STROKE, color),
    );
}

pub fn icon(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    // Some glyphs extend one design unit beyond their nominal 24-unit square.
    // Keep that plus the stroke in the clip test; interaction/labels are owned
    // by the surrounding widget and remain registered even when paint is culled.
    let margin = rect.width().min(rect.height()).max(0.) / 24. + 2.;
    if !painter.clip_rect().intersects(rect.expand(margin)) {
        return;
    }
    let stroke = if matches!(
        icon,
        Icon::Settings
            | Icon::Appearance
            | Icon::Equalizer
            | Icon::Tag
            | Icon::File
            | Icon::Keyboard
            | Icon::Bell
            | Icon::Backup
    ) {
        1.4
    } else {
        ICON_STROKE
    };
    match icon {
        Icon::Play => polygon(
            painter,
            rect,
            &[(7.5, 4.5), (18.5, 12.0), (7.5, 19.5)],
            color,
        ),
        Icon::Pause => {
            let side = rect.width().min(rect.height());
            for x in [7.0, 14.0] {
                let bar = Rect::from_min_max(point(rect, x, 5.0), point(rect, x + 3.0, 19.0));
                painter.rect_filled(
                    bar,
                    CornerRadius::same((side * 0.07).clamp(1.0, 2.0) as u8),
                    color,
                );
            }
        }
        Icon::Previous => {
            path(painter, rect, &[(4.0, 4.0), (4.0, 20.0)], color, stroke);
            polygon(
                painter,
                rect,
                &[(20.0, 4.0), (5.5, 12.0), (20.0, 20.0)],
                color,
            );
        }
        Icon::Next => {
            polygon(
                painter,
                rect,
                &[(4.0, 4.0), (18.5, 12.0), (4.0, 20.0)],
                color,
            );
            path(painter, rect, &[(20.0, 4.0), (20.0, 20.0)], color, stroke);
        }
        Icon::Stop => {
            let r = Rect::from_min_max(point(rect, 3.0, 3.0), point(rect, 21.0, 21.0));
            painter.rect_filled(r, CornerRadius::same(1), color);
        }
        Icon::Shuffle => {
            path(
                painter,
                rect,
                &[(4.0, 7.0), (8.0, 7.0), (16.0, 17.0), (20.0, 17.0)],
                color,
                stroke,
            );
            path(
                painter,
                rect,
                &[(4.0, 17.0), (8.0, 17.0), (16.0, 7.0), (20.0, 7.0)],
                color,
                stroke,
            );
            path(
                painter,
                rect,
                &[(16.5, 4.5), (20.5, 7.0), (16.5, 9.5)],
                color,
                stroke,
            );
            path(
                painter,
                rect,
                &[(16.5, 14.5), (20.5, 17.0), (16.5, 19.5)],
                color,
                stroke,
            );
        }
        Icon::Repeat => {
            path(
                painter,
                rect,
                &[
                    (5.0, 9.0),
                    (7.0, 6.5),
                    (16.5, 6.5),
                    (19.0, 9.0),
                    (19.0, 11.0),
                ],
                color,
                stroke,
            );
            path(
                painter,
                rect,
                &[(15.5, 8.5), (19.0, 11.0), (22.0, 8.5)],
                color,
                stroke,
            );
            path(
                painter,
                rect,
                &[
                    (19.0, 15.0),
                    (17.0, 17.5),
                    (7.5, 17.5),
                    (5.0, 15.0),
                    (5.0, 13.0),
                ],
                color,
                stroke,
            );
            path(
                painter,
                rect,
                &[(8.5, 15.5), (5.0, 13.0), (2.0, 15.5)],
                color,
                stroke,
            );
        }
        Icon::Bookmark => {
            path(
                painter,
                rect,
                &[
                    (7.0, 4.5),
                    (17.0, 4.5),
                    (17.0, 20.0),
                    (12.0, 16.5),
                    (7.0, 20.0),
                    (7.0, 4.5),
                ],
                color,
                stroke,
            );
        }
        Icon::Moon => {
            path(
                painter,
                rect,
                &[
                    (16.5, 4.0),
                    (14.2, 4.3),
                    (11.7, 5.8),
                    (9.7, 8.2),
                    (8.8, 11.2),
                    (9.3, 14.4),
                    (11.0, 17.0),
                    (13.8, 18.8),
                    (17.0, 19.0),
                    (19.3, 18.0),
                    (16.6, 16.7),
                    (14.8, 14.5),
                    (14.0, 11.8),
                    (14.4, 8.9),
                    (16.0, 6.6),
                    (18.4, 5.2),
                    (16.5, 4.0),
                ],
                color,
                stroke,
            );
        }
        Icon::Volume => {
            path(
                painter,
                rect,
                &[
                    (4.0, 9.0),
                    (7.5, 9.0),
                    (12.0, 5.5),
                    (12.0, 18.5),
                    (7.5, 15.0),
                    (4.0, 15.0),
                    (4.0, 9.0),
                ],
                color,
                stroke,
            );
            path(
                painter,
                rect,
                &[(15.0, 8.5), (17.0, 10.0), (17.0, 14.0), (15.0, 15.5)],
                color,
                stroke,
            );
            path(
                painter,
                rect,
                &[
                    (17.5, 6.0),
                    (20.5, 8.5),
                    (21.5, 12.0),
                    (20.5, 15.5),
                    (17.5, 18.0),
                ],
                color,
                stroke,
            );
        }
        Icon::Muted => {
            path(
                painter,
                rect,
                &[
                    (3.5, 9.0),
                    (7.5, 9.0),
                    (12.0, 5.5),
                    (12.0, 18.5),
                    (7.5, 15.0),
                    (3.5, 15.0),
                    (3.5, 9.0),
                ],
                color,
                stroke,
            );
            path(painter, rect, &[(16.0, 9.0), (21.0, 15.0)], color, stroke);
            path(painter, rect, &[(21.0, 9.0), (16.0, 15.0)], color, stroke);
        }
        Icon::Plus => {
            line(painter, rect, (12.0, 5.0), (12.0, 19.0), color);
            line(painter, rect, (5.0, 12.0), (19.0, 12.0), color);
        }
        Icon::Folder => {
            path(
                painter,
                rect,
                &[
                    (3.5, 7.0),
                    (9.0, 7.0),
                    (11.0, 9.0),
                    (20.5, 9.0),
                    (20.5, 18.5),
                    (3.5, 18.5),
                    (3.5, 7.0),
                ],
                color,
                stroke,
            );
            line(painter, rect, (3.5, 10.0), (20.5, 10.0), color);
        }
        Icon::Playlist => {
            for (y, end) in [(6.5, 19.5), (12.0, 18.0), (17.5, 16.5)] {
                painter.circle_filled(
                    point(rect, 4.5, y),
                    rect.width().min(rect.height()) * 0.035,
                    color,
                );
                line(painter, rect, (8.0, y), (end, y), color);
            }
        }
        Icon::Music => {
            path(
                painter,
                rect,
                &[(13.0, 16.0), (13.0, 5.0), (20.0, 3.5), (20.0, 14.0)],
                color,
                stroke,
            );
            line(painter, rect, (13.0, 7.0), (20.0, 5.5), color);
            painter.circle_filled(
                point(rect, 9.5, 17.0),
                rect.width().min(rect.height()) * 0.095,
                color,
            );
            painter.circle_filled(
                point(rect, 16.5, 15.0),
                rect.width().min(rect.height()) * 0.095,
                color,
            );
        }
        Icon::Book => {
            path(
                painter,
                rect,
                &[
                    (12.0, 6.0),
                    (9.5, 4.5),
                    (4.5, 4.5),
                    (4.5, 18.5),
                    (9.5, 18.5),
                    (12.0, 20.0),
                    (14.5, 18.5),
                    (19.5, 18.5),
                    (19.5, 4.5),
                    (14.5, 4.5),
                    (12.0, 6.0),
                ],
                color,
                stroke,
            );
            path(painter, rect, &[(12.0, 6.0), (12.0, 20.0)], color, stroke);
            path(painter, rect, &[(7.0, 8.0), (10.0, 8.0)], color, stroke);
            path(painter, rect, &[(14.0, 8.0), (17.0, 8.0)], color, stroke);
        }
        Icon::Clock => {
            painter.circle_stroke(
                point(rect, 12.0, 12.0),
                rect.width().min(rect.height()) * 0.34,
                Stroke::new(stroke, color),
            );
            line(painter, rect, (12.0, 7.0), (12.0, 12.0), color);
            line(painter, rect, (12.0, 12.0), (16.0, 14.0), color);
        }
        Icon::Continue => {
            path(
                painter,
                rect,
                &[
                    (18.5, 8.5),
                    (17.0, 5.5),
                    (13.5, 4.0),
                    (9.5, 4.2),
                    (6.0, 6.5),
                    (4.5, 10.0),
                    (5.0, 14.0),
                    (7.5, 17.0),
                    (11.0, 18.5),
                ],
                color,
                stroke,
            );
            path(
                painter,
                rect,
                &[(16.5, 7.0), (19.5, 8.5), (19.0, 5.0)],
                color,
                stroke,
            );
            polygon(
                painter,
                rect,
                &[(10.0, 9.0), (15.5, 12.0), (10.0, 15.0)],
                color,
            );
        }
        Icon::Check => {
            painter.circle_stroke(
                point(rect, 12.0, 12.0),
                rect.width().min(rect.height()) * 0.42,
                Stroke::new(stroke, color),
            );
            path(
                painter,
                rect,
                &[(7.5, 12.5), (10.5, 15.5), (16.5, 9.0)],
                color,
                stroke,
            );
        }
        Icon::Settings => {
            path(
                painter,
                rect,
                &[
                    (10.0, 3.0),
                    (14.0, 3.0),
                    (14.7, 5.3),
                    (16.5, 6.1),
                    (18.5, 5.2),
                    (21.0, 7.7),
                    (20.0, 9.6),
                    (20.7, 11.6),
                    (23.0, 12.3),
                    (23.0, 15.8),
                    (20.7, 16.4),
                    (20.0, 18.4),
                    (21.0, 20.3),
                    (18.5, 22.8),
                    (16.5, 21.9),
                    (14.7, 22.7),
                    (14.0, 25.0),
                    (10.5, 25.0),
                    (9.8, 22.7),
                    (7.8, 21.9),
                    (5.8, 22.8),
                    (3.3, 20.3),
                    (4.3, 18.4),
                    (3.6, 16.4),
                    (1.3, 15.8),
                    (1.3, 12.3),
                    (3.6, 11.6),
                    (4.3, 9.6),
                    (3.3, 7.7),
                    (5.8, 5.2),
                    (7.8, 6.1),
                    (9.8, 5.3),
                    (10.0, 3.0),
                ],
                color,
                stroke,
            );
            painter.circle_stroke(
                point(rect, 12.0, 14.0),
                rect.width().min(rect.height()) * 0.17,
                Stroke::new(stroke, color),
            );
        }
        Icon::Appearance => {
            path(
                painter,
                rect,
                &[
                    (12.0, 3.5),
                    (16.0, 4.2),
                    (19.0, 6.5),
                    (21.0, 10.0),
                    (21.0, 13.0),
                    (19.0, 15.0),
                    (16.0, 15.0),
                    (14.5, 17.0),
                    (13.5, 20.0),
                    (10.0, 20.5),
                    (6.5, 19.0),
                    (4.0, 16.0),
                    (3.0, 12.0),
                    (4.0, 8.0),
                    (7.0, 5.0),
                    (12.0, 3.5),
                ],
                color,
                stroke,
            );
            for (x, y) in [(8.0, 9.0), (13.0, 7.0), (17.0, 10.0), (8.0, 14.0)] {
                painter.circle_stroke(
                    point(rect, x, y),
                    rect.width().min(rect.height()) * 0.045,
                    Stroke::new(stroke * 0.85, color),
                );
            }
        }
        Icon::Equalizer => {
            for (x, y) in [(6.0, 9.0), (12.0, 15.0), (18.0, 7.0)] {
                path(painter, rect, &[(x, 4.0), (x, 20.0)], color, stroke);
                painter.circle_stroke(
                    point(rect, x, y),
                    rect.width().min(rect.height()) * 0.09,
                    Stroke::new(stroke, color),
                );
            }
        }
        Icon::Tag => {
            path(
                painter,
                rect,
                &[
                    (3.5, 5.0),
                    (14.0, 5.0),
                    (21.0, 12.0),
                    (13.0, 20.0),
                    (3.5, 10.5),
                    (3.5, 5.0),
                ],
                color,
                stroke,
            );
            painter.circle_stroke(
                point(rect, 8.0, 9.0),
                rect.width().min(rect.height()) * 0.055,
                Stroke::new(stroke * 0.85, color),
            );
        }
        Icon::File => {
            path(
                painter,
                rect,
                &[
                    (6.0, 3.5),
                    (15.0, 3.5),
                    (20.0, 8.5),
                    (20.0, 20.5),
                    (6.0, 20.5),
                    (6.0, 3.5),
                ],
                color,
                stroke,
            );
            path(
                painter,
                rect,
                &[(15.0, 3.5), (15.0, 8.5), (20.0, 8.5)],
                color,
                stroke,
            );
            path(painter, rect, &[(9.0, 12.0), (17.0, 12.0)], color, stroke);
            path(painter, rect, &[(9.0, 16.0), (17.0, 16.0)], color, stroke);
        }
        Icon::Keyboard => {
            let bounds = Rect::from_min_max(point(rect, 2.5, 5.5), point(rect, 21.5, 18.5));
            painter.rect_stroke(
                bounds,
                CornerRadius::same(2),
                Stroke::new(stroke, color),
                StrokeKind::Inside,
            );
            for row in 0..2 {
                for col in 0..5 {
                    let x = 5.0 + col as f32 * 3.2;
                    let y = 8.0 + row as f32 * 4.0;
                    let key = Rect::from_min_max(point(rect, x, y), point(rect, x + 1.8, y + 1.8));
                    painter.rect_stroke(
                        key,
                        CornerRadius::same(0),
                        Stroke::new(stroke * 0.7, color),
                        StrokeKind::Inside,
                    );
                }
            }
            let space = Rect::from_min_max(point(rect, 8.0, 16.0), point(rect, 16.0, 17.3));
            painter.rect_stroke(
                space,
                CornerRadius::same(0),
                Stroke::new(stroke * 0.7, color),
                StrokeKind::Inside,
            );
        }
        Icon::Bell => {
            path(
                painter,
                rect,
                &[
                    (5.0, 16.5),
                    (6.5, 14.5),
                    (7.0, 10.0),
                    (8.2, 7.0),
                    (10.5, 5.2),
                    (13.5, 5.2),
                    (15.8, 7.0),
                    (17.0, 10.0),
                    (17.5, 14.5),
                    (19.0, 16.5),
                    (5.0, 16.5),
                ],
                color,
                stroke,
            );
            path(
                painter,
                rect,
                &[(9.5, 19.0), (10.5, 20.0), (13.5, 20.0), (14.5, 19.0)],
                color,
                stroke,
            );
            painter.circle_stroke(
                point(rect, 12.0, 4.0),
                rect.width().min(rect.height()) * 0.035,
                Stroke::new(stroke * 0.8, color),
            );
        }
        Icon::Backup => {
            path(
                painter,
                rect,
                &[
                    (4.0, 6.0),
                    (12.0, 6.0),
                    (14.0, 8.0),
                    (18.0, 8.0),
                    (18.0, 16.0),
                ],
                color,
                stroke,
            );
            path(
                painter,
                rect,
                &[
                    (8.0, 10.0),
                    (19.0, 10.0),
                    (21.0, 12.0),
                    (21.0, 20.0),
                    (8.0, 20.0),
                    (8.0, 10.0),
                ],
                color,
                stroke,
            );
            path(
                painter,
                rect,
                &[(4.0, 6.0), (4.0, 16.0), (8.0, 20.0)],
                color,
                stroke,
            );
            path(
                painter,
                rect,
                &[(12.0, 15.0), (14.5, 12.5), (17.0, 15.0)],
                color,
                stroke,
            );
            path(painter, rect, &[(14.5, 12.5), (14.5, 17.5)], color, stroke);
        }
        Icon::Expand => {
            path(
                painter,
                rect,
                &[(9.0, 4.5), (4.5, 4.5), (4.5, 9.0)],
                color,
                stroke,
            );
            line(painter, rect, (4.5, 4.5), (10.0, 10.0), color);
            path(
                painter,
                rect,
                &[(15.0, 4.5), (19.5, 4.5), (19.5, 9.0)],
                color,
                stroke,
            );
            line(painter, rect, (19.5, 4.5), (14.0, 10.0), color);
            path(
                painter,
                rect,
                &[(4.5, 15.0), (4.5, 19.5), (9.0, 19.5)],
                color,
                stroke,
            );
            line(painter, rect, (4.5, 19.5), (10.0, 14.0), color);
            path(
                painter,
                rect,
                &[(15.0, 19.5), (19.5, 19.5), (19.5, 15.0)],
                color,
                stroke,
            );
            line(painter, rect, (19.5, 19.5), (14.0, 14.0), color);
        }
        Icon::Search => {
            painter.circle_stroke(
                point(rect, 10.5, 10.5),
                rect.width().min(rect.height()) * 0.25,
                Stroke::new(stroke, color),
            );
            line(painter, rect, (14.5, 14.5), (20.0, 20.0), color);
        }
        Icon::Radio => {
            path(
                painter,
                rect,
                &[
                    (4.0, 9.5),
                    (20.0, 9.5),
                    (20.0, 19.5),
                    (4.0, 19.5),
                    (4.0, 9.5),
                ],
                color,
                stroke,
            );
            line(painter, rect, (8.0, 9.5), (17.0, 4.5), color);
            painter.circle_stroke(
                point(rect, 9.5, 14.5),
                rect.width().min(rect.height()) * 0.1,
                Stroke::new(stroke, color),
            );
            line(painter, rect, (14.5, 12.5), (17.5, 12.5), color);
            line(painter, rect, (14.5, 16.5), (17.5, 16.5), color);
        }
        Icon::Artist => {
            painter.circle_stroke(
                point(rect, 12.0, 8.0),
                rect.width().min(rect.height()) * 0.17,
                Stroke::new(stroke, color),
            );
            path(
                painter,
                rect,
                &[
                    (4.5, 20.5),
                    (5.5, 16.5),
                    (9.0, 14.0),
                    (15.0, 14.0),
                    (18.5, 16.5),
                    (19.5, 20.5),
                ],
                color,
                stroke,
            );
        }
        Icon::Album => {
            painter.circle_stroke(
                point(rect, 12.0, 12.0),
                rect.width().min(rect.height()) * 0.42,
                Stroke::new(stroke, color),
            );
            painter.circle_stroke(
                point(rect, 12.0, 12.0),
                rect.width().min(rect.height()) * 0.1,
                Stroke::new(stroke, color),
            );
        }
    }
}

fn label_info(enabled: bool, selected: bool, label: &str) -> WidgetInfo {
    WidgetInfo::selected(WidgetType::Button, enabled, selected, label)
}

fn response_button(ui: &mut Ui, size: Vec2, label: &str, selected: bool) -> Response {
    let response = ui.add(
        Button::new(" ")
            .min_size(size)
            .frame(false)
            .sense(Sense::click()),
    );
    response.widget_info(|| label_info(ui.is_enabled(), selected, label));
    response
}

pub fn window_control(ui: &mut Ui, kind: WindowControl, width: f32, height: f32) -> Response {
    let tip = match kind {
        WindowControl::Minimize => "Minimize window",
        WindowControl::Maximize => "Maximize window",
        WindowControl::Restore => "Restore window",
        WindowControl::Close => "Close window",
    };
    let response = response_button(ui, Vec2::new(width, height), tip, false);
    let rect = response.rect;
    let visuals = ui.visuals().clone();
    let close_hot = kind == WindowControl::Close && response.hovered();
    let close_pressed = kind == WindowControl::Close && response.is_pointer_button_down_on();
    if close_hot || close_pressed {
        let red = if close_pressed {
            Color32::from_rgb(196, 15, 30)
        } else {
            Color32::from_rgb(232, 17, 35)
        };
        ui.painter().rect_filled(rect, CornerRadius::same(3), red);
    } else if response.hovered() || response.is_pointer_button_down_on() {
        let fill = if response.is_pointer_button_down_on() {
            visuals.widgets.active.bg_fill
        } else {
            visuals.widgets.hovered.bg_fill
        };
        ui.painter().rect_filled(rect, CornerRadius::same(3), fill);
    }

    let glyph_rect = Rect::from_center_size(rect.center(), Vec2::splat(24.0));
    let glyph = if close_hot || close_pressed {
        Color32::WHITE
    } else {
        visuals.text_color()
    };
    let stroke = Stroke::new(1.1, glyph);
    match kind {
        WindowControl::Minimize => {
            ui.painter().line_segment(
                [point(glyph_rect, 6.0, 12.0), point(glyph_rect, 18.0, 12.0)],
                stroke,
            );
        }
        WindowControl::Maximize => {
            let square =
                Rect::from_min_max(point(glyph_rect, 6.0, 6.0), point(glyph_rect, 18.0, 18.0));
            ui.painter()
                .rect_stroke(square, CornerRadius::ZERO, stroke, StrokeKind::Inside);
        }
        WindowControl::Restore => {
            // Only the visible edges of the back square are drawn, so the front
            // square needs no background fill of its own (the title bar and the
            // hover fill differ from the panel color).
            path(
                ui.painter(),
                glyph_rect,
                &[(9.5, 9.0), (9.5, 5.5), (17.5, 5.5), (17.5, 13.5), (14.0, 13.5)],
                glyph,
                1.1,
            );
            let front =
                Rect::from_min_max(point(glyph_rect, 5.0, 9.0), point(glyph_rect, 14.0, 18.0));
            ui.painter()
                .rect_stroke(front, CornerRadius::ZERO, stroke, StrokeKind::Inside);
        }
        WindowControl::Close => {
            ui.painter().line_segment(
                [point(glyph_rect, 7.0, 7.0), point(glyph_rect, 17.0, 17.0)],
                stroke,
            );
            ui.painter().line_segment(
                [point(glyph_rect, 17.0, 7.0), point(glyph_rect, 7.0, 17.0)],
                stroke,
            );
        }
    }
    if response.has_focus() {
        focus_ring(
            ui.painter(),
            rect,
            false,
            if close_hot {
                Color32::WHITE
            } else {
                visuals.hyperlink_color
            },
        );
    }
    response.on_hover_text(tip)
}

/// Base colours of the generic album covers: eight muted mid-tones. The light
/// theme shows them as pale tints, the dark theme as deep ones.
const ART_BASES: [[u8; 3]; 8] = [
    [86, 112, 150],
    [70, 128, 128],
    [96, 138, 98],
    [150, 132, 78],
    [164, 102, 88],
    [148, 94, 120],
    [118, 98, 150],
    [104, 116, 130],
];

fn mix_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let channel = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color32::from_rgb(
        channel(a.r(), b.r()),
        channel(a.g(), b.g()),
        channel(a.b(), b.b()),
    )
}

/// FNV-1a over the trimmed, lower-cased seed, folded onto the palette.
fn art_index(seed: &str) -> usize {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut buffer = [0u8; 4];
    for ch in seed.trim().chars().flat_map(char::to_lowercase) {
        for byte in ch.encode_utf8(&mut buffer).bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    ((hash ^ (hash >> 32)) % ART_BASES.len() as u64) as usize
}

fn art_palette_color(index: usize, dark: bool) -> Color32 {
    let [r, g, b] = ART_BASES[index % ART_BASES.len()];
    let base = Color32::from_rgb(r, g, b);
    if dark {
        mix_color(base, Color32::BLACK, 0.45)
    } else {
        mix_color(base, Color32::WHITE, 0.62)
    }
}

/// Top colour of a generic cover: one of eight muted tints picked from `seed`
/// (the album, else the folder name, else the title). The same seed always
/// gives the same colour; an empty seed is fine.
pub fn art_tint(seed: &str, dark: bool) -> Color32 {
    art_palette_color(art_index(seed), dark)
}

/// Bottom colour of the gradient: the tint, slightly darker.
fn art_shade(tint: Color32) -> Color32 {
    mix_color(tint, Color32::BLACK, 0.14)
}

/// Glyph colour: the tint pushed toward the opposite end of the lightness range.
fn art_glyph_color(tint: Color32, dark: bool) -> Color32 {
    if dark {
        mix_color(tint, Color32::WHITE, 0.56)
    } else {
        mix_color(tint, Color32::BLACK, 0.64)
    }
}

/// Vertical gradient for a rounded rectangle. The mesh rows follow the corner
/// arcs, so the corners stay round at every size.
fn rounded_art_gradient(rect: Rect, top: Color32, bottom: Color32, radius: f32) -> Shape {
    const ARC: usize = 5;
    let radius = radius.clamp(0.0, rect.width().min(rect.height()) * 0.5);
    let arc = |step: usize| {
        let angle = std::f32::consts::FRAC_PI_2 * step as f32 / ARC as f32;
        (radius * (1.0 - angle.cos()), radius * (1.0 - angle.sin()))
    };
    let mut rows = Vec::with_capacity(2 * (ARC + 1));
    for step in 0..=ARC {
        let (down, inset) = arc(step);
        rows.push((rect.top() + down, inset));
    }
    for step in (0..=ARC).rev() {
        let (up, inset) = arc(step);
        rows.push((rect.bottom() - up, inset));
    }
    let mut mesh = Mesh::default();
    for (row, &(y, inset)) in rows.iter().enumerate() {
        let t = ((y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0);
        let color = mix_color(top, bottom, t);
        mesh.colored_vertex(Pos2::new(rect.left() + inset, y), color);
        mesh.colored_vertex(Pos2::new(rect.right() - inset, y), color);
        if row > 0 {
            let previous = (row as u32 - 1) * 2;
            let current = row as u32 * 2;
            mesh.add_triangle(previous, previous + 1, current);
            mesh.add_triangle(previous + 1, current + 1, current);
        }
    }
    Shape::mesh(mesh)
}

/// Corner radius of a cover tile (and of a real thumbnail beside it).
pub fn art_corner_radius(side: f32) -> f32 {
    (side * 0.08).max(3.0)
}

/// Generic album cover for music without artwork: a rounded tile with a muted
/// per-album vertical gradient, a note (a book for audiobooks) and a faint
/// border. `rect` should be square; nothing here needs an asset.
pub fn paint_default_art(painter: &Painter, rect: Rect, audiobook: bool, seed: &str, dark: bool) {
    paint_art(
        painter,
        rect,
        if audiobook { Icon::Book } else { Icon::Music },
        !audiobook,
        seed,
        dark,
    );
}

/// The generic cover of an internet radio station: the same tile with a small
/// radio set. `seed` (the station's name) tints it.
pub fn paint_station_art(painter: &Painter, rect: Rect, seed: &str, dark: bool) {
    paint_art(painter, rect, Icon::Radio, false, seed, dark);
}

/// A generic cover tile with `glyph` in it. `nudge` moves the glyph left a
/// little: the music note's mass sits right of its 24-unit box.
fn paint_art(painter: &Painter, rect: Rect, glyph_kind: Icon, nudge: bool, seed: &str, dark: bool) {
    let side = rect.width().min(rect.height());
    if side < 1.0 || !painter.clip_rect().intersects(rect) {
        return;
    }
    let tint = art_tint(seed, dark);
    let glyph = art_glyph_color(tint, dark);
    let radius = art_corner_radius(side);
    painter.add(rounded_art_gradient(rect, tint, art_shade(tint), radius));
    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(glyph.r(), glyph.g(), glyph.b(), 64),
        ),
        StrokeKind::Inside,
    );
    let size = (side * 0.5).min(72.0);
    if size >= 10.0 {
        let nudge = if nudge { size * 0.06 } else { 0.0 };
        icon(
            painter,
            Rect::from_center_size(rect.center() - Vec2::new(nudge, 0.0), Vec2::splat(size)),
            glyph_kind,
            glyph,
        );
    }
}





fn focus_ring(painter: &Painter, rect: Rect, circular: bool, color: Color32) {
    let stroke = Stroke::new(1.5, color);
    if circular {
        painter.circle_stroke(
            rect.center(),
            rect.width().min(rect.height()) * 0.5 - 1.0,
            stroke,
        );
    } else {
        painter.rect_stroke(rect, CornerRadius::same(6), stroke, StrokeKind::Inside);
    }
}

fn palette_rgb([red, green, blue]: [u8; 3]) -> Color32 {
    Color32::from_rgb(red, green, blue)
}

pub fn icon_button(
    ui: &mut Ui,
    icon_kind: Icon,
    tip: &str,
    selected: bool,
    circular: bool,
    size: f32,
) -> Response {
    let response = response_button(ui, Vec2::splat(size), tip, selected);
    let rect = response.rect;
    let visuals = ui.visuals();
    let accent = visuals.hyperlink_color;
    let glyph = if selected {
        accent
    } else {
        visuals.text_color()
    };

    if circular {
        let palette = crate::appearance::custom_palette(ui);
        let radius = rect.width().min(rect.height()) * 0.5 - 1.0;
        let pressed = response.is_pointer_button_down_on();
        let inner = if let Some(palette) = palette {
            palette_rgb(if pressed { palette.panel2 } else { palette.panel })
        } else if visuals.dark_mode && pressed {
            Color32::from_rgb(67, 75, 86)
        } else if visuals.dark_mode {
            Color32::from_rgb(48, 55, 65)
        } else if pressed {
            Color32::from_rgb(237, 240, 244)
        } else {
            Color32::from_rgb(252, 253, 254)
        };
        painter_circle_gradient(
            ui.painter(),
            rect.center(),
            radius,
            inner,
            visuals.dark_mode,
            palette,
        );
        ui.painter().circle_stroke(
            rect.center(),
            radius,
            Stroke::new(
                1.0,
                if let Some(palette) = palette {
                    palette_rgb(palette.scroll)
                } else if visuals.dark_mode {
                    Color32::from_rgb(105, 115, 129)
                } else {
                    Color32::from_rgb(188, 196, 206)
                },
            ),
        );
        ui.painter().circle_stroke(
            rect.center(),
            (radius - 2.0).max(0.0),
            Stroke::new(
                0.7,
                if let Some(palette) = palette {
                    palette_rgb(palette.scroll_hi)
                } else if visuals.dark_mode {
                    Color32::from_rgb(112, 121, 134)
                } else {
                    Color32::from_rgb(255, 255, 255)
                },
            ),
        );
        if response.hovered() {
            ui.painter().circle_stroke(
                rect.center(),
                radius + 1.0,
                Stroke::new(1.0, accent.linear_multiply(0.5)),
            );
        }
    } else {
        let fill = if response.is_pointer_button_down_on() {
            visuals.widgets.active.bg_fill
        } else if response.hovered() {
            visuals.widgets.hovered.bg_fill
        } else if selected {
            accent.linear_multiply(0.12)
        } else {
            Color32::TRANSPARENT
        };
        if fill != Color32::TRANSPARENT {
            ui.painter().rect_filled(rect, CornerRadius::same(6), fill);
        }
    }

    let icon_size = if circular {
        (size * 0.55).min(32.0)
    } else {
        (size * 0.60).min(24.0)
    };
    let icon_rect = Rect::from_center_size(rect.center(), Vec2::splat(icon_size));
    icon(ui.painter(), icon_rect, icon_kind, glyph);
    if response.has_focus() {
        focus_ring(ui.painter(), rect, circular, accent);
    }
    response.on_hover_text(tip)
}

pub fn skip_button(ui: &mut Ui, seconds: u32, forward: bool, size: f32) -> Response {
    let direction = if forward { "Forward" } else { "Back" };
    let tip = format!("{direction} {seconds} seconds");
    let response = response_button(ui, Vec2::splat(size), &tip, false);
    let rect = response.rect;
    let visuals = ui.visuals().clone();
    let color = visuals.text_color();

    if response.hovered() || response.is_pointer_button_down_on() {
        let fill = if response.is_pointer_button_down_on() {
            visuals.hyperlink_color.linear_multiply(0.14)
        } else {
            visuals.widgets.hovered.bg_fill
        };
        ui.painter().circle_filled(
            rect.center(),
            rect.width().min(rect.height()) * 0.5 - 1.0,
            fill,
        );
    }

    let icon_rect = Rect::from_center_size(rect.center(), Vec2::splat(size * 0.62));
    let map_x = |x: f32| if forward { x } else { 24.0 - x };
    let ring = [
        (map_x(17.2), 5.0),
        (map_x(14.5), 3.7),
        (map_x(11.0), 3.5),
        (map_x(7.5), 4.8),
        (map_x(5.0), 7.5),
        (map_x(3.8), 11.0),
        (map_x(4.5), 14.6),
        (map_x(7.0), 17.5),
        (map_x(10.5), 19.0),
        (map_x(14.0), 18.5),
    ];
    path(ui.painter(), icon_rect, &ring, color, 1.45);
    path(
        ui.painter(),
        icon_rect,
        &[(map_x(16.0), 2.8), (map_x(20.0), 4.2), (map_x(19.5), 8.1)],
        color,
        1.45,
    );
    ui.painter().text(
        icon_rect.center() + Vec2::new(0.0, 0.4),
        egui::Align2::CENTER_CENTER,
        seconds.to_string(),
        FontId::proportional((size * 0.28).clamp(10.0, 12.0)),
        color,
    );
    if response.has_focus() {
        focus_ring(ui.painter(), rect, true, ui.visuals().hyperlink_color);
    }
    response.on_hover_text(tip)
}

fn painter_circle_gradient(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    inner: Color32,
    dark_mode: bool,
    palette: Option<&crate::themes::Palette>,
) {
    // Three close rings keep the play control bright while giving its edge a
    // brushed-silver falloff without a bitmap asset.
    let (rim, middle) = if let Some(palette) = palette {
        (palette_rgb(palette.scroll), palette_rgb(palette.panel2))
    } else if dark_mode {
        (
            Color32::from_rgb(83, 92, 105),
            Color32::from_rgb(57, 64, 74),
        )
    } else {
        (
            Color32::from_rgb(218, 223, 230),
            Color32::from_rgb(246, 248, 250),
        )
    };
    painter.circle_filled(center, radius, rim);
    painter.circle_filled(center, (radius - 1.0).max(0.0), middle);
    painter.circle_filled(center, (radius - 2.0).max(0.0), inner);
}

fn rounded_gradient(rect: Rect, top: Color32, bottom: Color32, radius: f32) -> Shape {
    const ROWS: usize = 8;
    let radius = radius.min(rect.width() * 0.5).min(rect.height() * 0.5);
    let mut mesh = Mesh::default();
    for row in 0..=ROWS {
        let y = rect.top() + rect.height() * row as f32 / ROWS as f32;
        let top_edge = y - rect.top();
        let bottom_edge = rect.bottom() - y;
        let inset = if top_edge < radius {
            radius
                - (radius * radius - (radius - top_edge).powi(2))
                    .max(0.0)
                    .sqrt()
        } else if bottom_edge < radius {
            radius
                - (radius * radius - (radius - bottom_edge).powi(2))
                    .max(0.0)
                    .sqrt()
        } else {
            0.0
        };
        let t = (row as f32 / ROWS as f32).clamp(0.0, 1.0);
        let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        let color = Color32::from_rgb(
            mix(top.r(), bottom.r()),
            mix(top.g(), bottom.g()),
            mix(top.b(), bottom.b()),
        );
        mesh.colored_vertex(Pos2::new(rect.left() + inset, y), color);
        mesh.colored_vertex(Pos2::new(rect.right() - inset, y), color);
        if row > 0 {
            let previous = (row as u32 - 1) * 2;
            let current = row as u32 * 2;
            mesh.add_triangle(previous, previous + 1, current);
            mesh.add_triangle(previous + 1, current + 1, current);
        }
    }
    Shape::mesh(mesh)
}

fn outlined_button_surface(
    ui: &Ui,
    rect: Rect,
    visuals: &egui::Visuals,
    pressed: bool,
    hovered: bool,
) {
    if pressed {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(7), visuals.widgets.active.bg_fill);
    } else if hovered {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(7), visuals.widgets.hovered.bg_fill);
    } else {
        ui.painter().rect_filled(
            rect.translate(Vec2::new(0.0, 1.0)),
            CornerRadius::same(7),
            Color32::from_black_alpha(12),
        );
        let (top, bottom) = if let Some(palette) = crate::appearance::custom_palette(ui) {
            (palette_rgb(palette.panel), palette_rgb(palette.panel2))
        } else if visuals.dark_mode {
            (Color32::from_rgb(55, 61, 70), Color32::from_rgb(43, 49, 57))
        } else {
            (Color32::WHITE, Color32::from_rgb(243, 245, 248))
        };
        ui.painter().add(rounded_gradient(rect, top, bottom, 7.0));
    }
    let border = if visuals.dark_mode || crate::appearance::custom_palette(ui).is_some() {
        visuals.widgets.inactive.bg_stroke
    } else {
        Stroke::new(1.0, Color32::from_rgb(203, 211, 223))
    };
    ui.painter()
        .rect_stroke(rect, CornerRadius::same(7), border, StrokeKind::Inside);
}

pub fn text_icon_button(ui: &mut Ui, icon_kind: Icon, label: &str) -> Response {
    let visuals = ui.visuals().clone();
    let color = visuals.text_color();
    let font = egui::TextStyle::Body.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, color);
    let size = Vec2::new(galley.size().x + 46.0, (galley.size().y + 12.0).max(34.0));
    let response = response_button(ui, size, label, false);
    let rect = response.rect;
    let foreground = if response.is_pointer_button_down_on() {
        visuals.widgets.active.fg_stroke.color
    } else {
        color
    };
    outlined_button_surface(
        ui,
        rect,
        &visuals,
        response.is_pointer_button_down_on(),
        response.hovered(),
    );
    let icon_rect = Rect::from_min_size(
        Pos2::new(rect.left() + 10.0, rect.center().y - 9.0),
        Vec2::splat(18.0),
    );
    icon(ui.painter(), icon_rect, icon_kind, foreground);
    ui.painter().galley_with_override_text_color(
        Pos2::new(rect.left() + 34.0, rect.center().y - galley.size().y * 0.5),
        galley,
        foreground,
    );
    if response.has_focus() {
        focus_ring(ui.painter(), rect, false, visuals.hyperlink_color);
    }
    response
}

pub fn menu_icon_button(ui: &mut Ui, icon_kind: Icon, label: &str) -> Response {
    let visuals = ui.visuals().clone();
    let color = visuals.text_color();
    let font = egui::TextStyle::Body.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, color);
    let size = Vec2::new(galley.size().x + 60.0, (galley.size().y + 12.0).max(34.0));
    let response = response_button(ui, size, label, false);
    let rect = response.rect;
    let foreground = if response.is_pointer_button_down_on() {
        visuals.widgets.active.fg_stroke.color
    } else {
        color
    };
    outlined_button_surface(
        ui,
        rect,
        &visuals,
        response.is_pointer_button_down_on(),
        response.hovered(),
    );
    let icon_rect = Rect::from_min_size(
        Pos2::new(rect.left() + 10.0, rect.center().y - 9.0),
        Vec2::splat(18.0),
    );
    icon(ui.painter(), icon_rect, icon_kind, foreground);
    ui.painter().galley_with_override_text_color(
        Pos2::new(rect.left() + 34.0, rect.center().y - galley.size().y * 0.5),
        galley,
        foreground,
    );
    let chevron_rect = Rect::from_center_size(
        Pos2::new(rect.right() - 12.0, rect.center().y),
        Vec2::splat(10.0),
    );
    path(
        ui.painter(),
        chevron_rect,
        &[(5.0, 7.0), (12.0, 14.0), (19.0, 7.0)],
        foreground,
        1.4,
    );
    if response.has_focus() {
        focus_ring(ui.painter(), rect, false, visuals.hyperlink_color);
    }
    response
}

/// A compact action button with the same raised surface as the icon buttons.
pub fn text_button(ui: &mut Ui, label: &str) -> Response {
    let visuals = ui.visuals().clone();
    let color = visuals.text_color();
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        egui::TextStyle::Body.resolve(ui.style()),
        color,
    );
    let size = Vec2::new(galley.size().x + 20., (galley.size().y + 12.).max(34.));
    let response = response_button(ui, size, label, false);
    let color = if crate::appearance::custom_palette(ui).is_some()
        && response.is_pointer_button_down_on() {
        visuals.widgets.active.fg_stroke.color
    } else { color };
    let rect = response.rect;
    outlined_button_surface(
        ui,
        rect,
        &visuals,
        response.is_pointer_button_down_on(),
        response.hovered(),
    );
    ui.painter().galley_with_override_text_color(
        Pos2::new(
            rect.center().x - galley.size().x * 0.5,
            rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        color,
    );
    if response.has_focus() {
        focus_ring(ui.painter(), rect, false, visuals.hyperlink_color);
    }
    response
}

/// A compact menu button with the same raised surface as the icon buttons.
pub fn text_menu_button(ui: &mut Ui, label: &str) -> Response {
    text_menu_button_bounded(ui, label, f32::INFINITY)
}

/// The same menu button with a width limit and a single ellipsized label.
pub fn text_menu_button_bounded(ui: &mut Ui, label: &str, max_width: f32) -> Response {
    let visuals = ui.visuals().clone();
    let color = visuals.text_color();
    let font = egui::TextStyle::Body.resolve(ui.style());
    let mut galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        font.clone(),
        color,
    );
    let size = Vec2::new(
        (galley.size().x + 28.).min(max_width.max(1.)),
        (galley.size().y + 12.).max(34.),
    );
    if galley.size().x > (size.x - 28.).max(0.) {
        let mut job = egui::text::LayoutJob::simple_singleline(label.to_owned(), font, color);
        job.wrap = egui::text::TextWrapping::truncate_at_width((size.x - 28.).max(0.));
        galley = ui.painter().layout_job(job);
    }
    let response = response_button(ui, size, label, false);
    let color = if crate::appearance::custom_palette(ui).is_some()
        && response.is_pointer_button_down_on() {
        visuals.widgets.active.fg_stroke.color
    } else { color };
    let rect = response.rect;
    outlined_button_surface(
        ui,
        rect,
        &visuals,
        response.is_pointer_button_down_on(),
        response.hovered(),
    );
    if size.x >= 28. {
        ui.painter().galley_with_override_text_color(
            Pos2::new(rect.left() + 8., rect.center().y - galley.size().y * 0.5),
            galley,
            color,
        );
    }
    let chevron = Rect::from_center_size(
        Pos2::new(if size.x >= 28. { rect.right() - 8. } else { rect.center().x }, rect.center().y),
        Vec2::splat(8f32.min(size.x)),
    );
    path(
        ui.painter(),
        chevron,
        &[(5., 7.), (12., 14.), (19., 7.)],
        color,
        1.4,
    );
    if response.has_focus() {
        focus_ring(ui.painter(), rect, false, visuals.hyperlink_color);
    }
    response
}

pub fn tab(ui: &mut Ui, label: &str, selected: bool) -> Response {
    let visuals = ui.visuals().clone();
    // Named palettes can use dark text on a light selected row, but tabs have
    // only an underline and must remain legible on the ordinary panel.
    let color = if selected && crate::appearance::custom_palette(ui).is_none() {
        crate::appearance::row_text(ui, true)
    } else {
        visuals.text_color()
    };
    let normal_font = egui::TextStyle::Body.resolve(ui.style());
    let font = if selected {
        crate::appearance::semibold(normal_font.size)
    } else {
        normal_font
    };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, color);
    let size = Vec2::new(galley.size().x + 24.0, (galley.size().y + 12.0).max(38.0));
    let response = response_button(ui, size, label, selected);
    let rect = response.rect;
    if response.hovered() && !selected {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(5), visuals.widgets.hovered.bg_fill);
    }
    ui.painter().galley(
        Pos2::new(
            rect.center().x - galley.size().x * 0.5,
            rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        color,
    );
    if selected {
        let underline = Rect::from_min_max(
            Pos2::new(rect.left() + 2.0, rect.bottom() - 2.5),
            Pos2::new(rect.right() - 2.0, rect.bottom()),
        );
        ui.painter()
            .rect_filled(underline, CornerRadius::same(1), visuals.hyperlink_color);
    }
    if response.has_focus() {
        focus_ring(ui.painter(), rect, false, visuals.hyperlink_color);
    }
    response
}

pub fn slider(ui: &mut Ui, value: &mut f64, min: f64, max: f64, width: f32, tip: &str) -> Response {
    slider_with_marks(ui, value, min, max, width, tip, &[])
}

/// Most marks a slider draws. More than this would be a comb, not a hint.
pub const MAX_SLIDER_MARKS: usize = 300;
/// A mark closer than this many pixels to the one before it, or to either end
/// of the track, is left out.
const MIN_MARK_GAP: f32 = 3.0;

/// Pixel x positions of the marks a slider draws, left to right. `marks` are
/// values in the slider's range, in ascending order; `left` and `right` are the
/// ends of the track. None are drawn when there are more than
/// `MAX_SLIDER_MARKS`; marks outside the range, not finite, or less than
/// `MIN_MARK_GAP` from the previous mark or from an end are skipped. It does not
/// allocate.
pub fn mark_positions(
    marks: &[f64],
    min: f64,
    max: f64,
    left: f32,
    right: f32,
) -> impl Iterator<Item = f32> + '_ {
    let span = max - min;
    let usable = marks.len() <= MAX_SLIDER_MARKS
        && min.is_finite()
        && max.is_finite()
        && span.is_finite()
        && span > 0.0
        && left.is_finite()
        && right.is_finite()
        && right > left;
    let mut previous = f32::NEG_INFINITY;
    marks
        .iter()
        .take(if usable { marks.len() } else { 0 })
        .filter_map(move |&mark| {
            let t = (mark - min) / span;
            if !(t > 0.0 && t < 1.0) {
                return None;
            }
            let x = left + t as f32 * (right - left);
            if x - left < MIN_MARK_GAP || right - x < MIN_MARK_GAP || x - previous < MIN_MARK_GAP {
                return None;
            }
            previous = x;
            Some(x)
        })
}

/// `slider` with thin tick marks at the given values (chapter starts on the
/// seek bar). The marks are only drawn: the hit area, keyboard use and value
/// handling are those of `slider`.
pub fn slider_with_marks(
    ui: &mut Ui,
    value: &mut f64,
    min: f64,
    max: f64,
    width: f32,
    tip: &str,
    marks: &[f64],
) -> Response {
    let (min, max) = if min.is_finite() && max.is_finite() && max > min && (max - min).is_finite() {
        (min, max)
    } else {
        (0.0, 1.0)
    };
    let mut response = ui
        .allocate_exact_size(Vec2::new(width.max(24.0), 26.0), Sense::click_and_drag())
        .1;
    if !value.is_finite() {
        *value = min;
    }
    *value = value.clamp(min, max);
    // Clamping a caller value that is out of range is not a user edit: compare
    // against the sanitized value so only keyboard or pointer input marks a change.
    let old_value = *value;

    let step = (max - min) * 0.01;
    let has_focus = response.has_focus();
    if has_focus {
        // The slider owns all four arrows; without this egui also moves focus to
        // the nearest neighbouring widget on every arrow press.
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                response.id,
                egui::EventFilter {
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    ..Default::default()
                },
            );
        });
    }
    let keyboard_step = ui.ctx().input(|input| {
        if !has_focus {
            return None;
        }
        if input.key_pressed(egui::Key::Home) {
            Some(min)
        } else if input.key_pressed(egui::Key::End) {
            Some(max)
        } else if input.key_pressed(egui::Key::ArrowLeft) || input.key_pressed(egui::Key::ArrowDown)
        {
            Some((*value - step).max(min))
        } else if input.key_pressed(egui::Key::ArrowRight) || input.key_pressed(egui::Key::ArrowUp)
        {
            Some((*value + step).min(max))
        } else {
            None
        }
    });
    if let Some(next) = keyboard_step {
        *value = next;
        // The first focus frame cannot install egui's next-frame lock yet.
        // Cancel only the cardinal move already queued for this arrow; a later
        // Tab in the same input batch must still move focus normally.
        let owns_move = ui.input(|input| input.events.iter().rev().find_map(|event| {
            match event {
                egui::Event::Key { key: egui::Key::Tab, pressed: true, modifiers, .. }
                    if !modifiers.any() || modifiers.shift_only() => Some(false),
                egui::Event::Key { key, pressed: true, modifiers, .. }
                    if !modifiers.any() && matches!(key, egui::Key::ArrowLeft | egui::Key::ArrowRight | egui::Key::ArrowUp | egui::Key::ArrowDown) => Some(true),
                _ => None,
            }
        }).unwrap_or(false));
        if owns_move { ui.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None)); }
    }

    let rect = response.rect;
    let inset = (rect.height() * 0.5).max(8.0);
    let left = rect.left() + inset;
    let right = (rect.right() - inset).max(left + 1.0);
    // Pointer use must not leave the slider holding the keyboard (it would
    // swallow Space and the arrow keys); Tab focus still works.
    if response.clicked_by(egui::PointerButton::Primary)
        || response.drag_stopped_by(egui::PointerButton::Primary)
    {
        response.surrender_focus();
    }
    // clicked() also includes Enter/Space and accessibility activation. Those
    // events must never turn an unrelated pointer hover into a seek/volume edit.
    if response.dragged_by(egui::PointerButton::Primary)
        || response.drag_stopped_by(egui::PointerButton::Primary)
        || response.clicked_by(egui::PointerButton::Primary)
    {
        if let Some(pointer) = response
            .interact_pointer_pos()
            .or_else(|| ui.ctx().input(|input| input.pointer.hover_pos()))
        {
            let t = ((pointer.x - left) / (right - left)).clamp(0.0, 1.0) as f64;
            *value = min + t * (max - min);
        }
    }
    *value = if value.is_finite() {
        value.clamp(min, max)
    } else {
        min
    };
    if *value != old_value {
        response.mark_changed();
    }
    response.widget_info(|| WidgetInfo::slider(ui.is_enabled(), *value, tip));

    let visuals = ui.visuals();
    let accent = visuals.hyperlink_color;
    let center_y = rect.center().y;
    let thumb_x = left + ((*value - min) / (max - min)) as f32 * (right - left);
    let palette = crate::appearance::custom_palette(ui);
    let track_color = if let Some(palette) = palette {
        palette_rgb(palette.groove)
    } else if visuals.dark_mode {
        Color32::from_gray(68)
    } else {
        Color32::from_rgb(211, 216, 223)
    };
    ui.painter().rect_filled(
        Rect::from_min_max(
            Pos2::new(left, center_y - 2.0),
            Pos2::new(right, center_y + 2.0),
        ),
        CornerRadius::same(2),
        track_color,
    );
    ui.painter().rect_filled(
        Rect::from_min_max(
            Pos2::new(left, center_y - 2.0),
            Pos2::new(thumb_x, center_y + 2.0),
        ),
        CornerRadius::same(2),
        accent,
    );
    if !marks.is_empty() {
        let tick_color = if let Some(palette) = palette {
            let color = palette_rgb(palette.text);
            Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 180)
        } else if visuals.dark_mode {
            Color32::from_white_alpha(150)
        } else {
            Color32::from_black_alpha(120)
        };
        for x in mark_positions(marks, min, max, left, right) {
            ui.painter().line_segment(
                [Pos2::new(x, center_y - 5.0), Pos2::new(x, center_y + 5.0)],
                Stroke::new(1.0, tick_color),
            );
        }
    }
    let thumb_center = Pos2::new(thumb_x, center_y);
    let radius = 8.0;
    ui.painter().circle_filled(
        thumb_center + Vec2::new(0.0, 1.0),
        radius + 0.6,
        Color32::from_black_alpha(28),
    );
    ui.painter()
        .circle_filled(thumb_center, radius, palette.map_or(Color32::WHITE, |p| palette_rgb(p.text)));
    ui.painter().circle_stroke(
        thumb_center,
        radius,
        Stroke::new(1.0, palette.map_or(Color32::from_rgb(170, 180, 191), |p| palette_rgb(p.scroll))),
    );
    if response.has_focus() {
        ui.painter()
            .circle_stroke(thumb_center, radius + 3.0, Stroke::new(1.25, accent));
    }
    response.on_hover_text(tip)
}
