use std::f32::consts::PI;

use egui::{vec2, Color32, CornerRadius, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Monitor,
    Window,
    Crop,
    Film,
    Sliders,
    Chip,
    Speaker,
    Microphone,
    Cursor,
    Camera,
    Timer,
    Hourglass,
    Folder,
    Refresh,
    Record,
    Stop,
    Pause,
    Play,
    Screenshot,
    Image,
    Trash,
    Check,
    Open,
    Keyboard,
    Alert,
    Close,
    Bolt,
    Mirror,
    Corner,
    Info,
    Copy,
    Scissors,
    Undo,
    Redo,
    Plus,
    Minus,
    SkipStart,
    SkipEnd,
    StepBack,
    StepForward,
    Export,
    Import,
    Mute,
    Fit,
    Duplicate,
    Edit,
    Save,
}

pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let side = rect.width().min(rect.height());
    let rect = Rect::from_center_size(rect.center(), Vec2::splat(side));
    let stroke = (side / 24.0 * 2.0).clamp(1.6, 2.6);
    let pen = Pen {
        painter,
        rect,
        stroke,
        color,
    };

    match icon {
        Icon::Monitor => {
            pen.rect_fill(2.5, 3.5, 19.0, 13.0, 2.0, pen.color);
            pen.rect_fill(10.8, 16.0, 2.4, 3.2, 0.0, pen.color);
            pen.rect_fill(7.5, 18.5, 9.0, 2.2, 1.0, pen.color);
        }
        Icon::Window => {
            pen.rect(3.0, 4.0, 18.0, 16.0, 2.0);
            pen.rect_fill(3.0, 4.0, 18.0, 5.0, 2.0, pen.color);
        }
        Icon::Crop => {
            pen.line(&[(8.0, 3.0), (4.0, 3.0), (4.0, 7.0)]);
            pen.line(&[(16.0, 3.0), (20.0, 3.0), (20.0, 7.0)]);
            pen.line(&[(4.0, 17.0), (4.0, 21.0), (8.0, 21.0)]);
            pen.line(&[(20.0, 17.0), (20.0, 21.0), (16.0, 21.0)]);
        }
        Icon::Film => {
            pen.rect(3.0, 5.0, 18.0, 14.0, 2.0);
            for x in [3.0, 17.5] {
                for y in [7.0, 10.75, 14.5] {
                    pen.rect_fill(x + 0.6, y, 2.4, 2.2, 0.4, pen.color);
                }
            }
            pen.polygon(&[(9.8, 8.8), (9.8, 15.2), (15.0, 12.0)], pen.color);
        }
        Icon::Sliders => {
            pen.line(&[(4.0, 7.0), (20.0, 7.0)]);
            pen.circle((9.0, 7.0), 2.1);
            pen.line(&[(4.0, 12.0), (20.0, 12.0)]);
            pen.circle((15.0, 12.0), 2.1);
            pen.line(&[(4.0, 17.0), (20.0, 17.0)]);
            pen.circle((7.5, 17.0), 2.1);
        }
        Icon::Chip => {
            pen.rect(7.0, 7.0, 10.0, 10.0, 2.0);
            for x in [9.5, 12.0, 14.5] {
                pen.line(&[(x, 3.5), (x, 7.0)]);
                pen.line(&[(x, 17.0), (x, 20.5)]);
            }
            for y in [9.5, 12.0, 14.5] {
                pen.line(&[(3.5, y), (7.0, y)]);
                pen.line(&[(17.0, y), (20.5, y)]);
            }
        }
        Icon::Speaker => {
            pen.rect_fill(3.0, 8.8, 5.5, 6.4, 1.0, pen.color);
            pen.polygon(
                &[(7.5, 8.8), (12.5, 4.5), (12.5, 19.5), (7.5, 15.2)],
                pen.color,
            );
            pen.line(&[(15.0, 9.5), (16.6, 11.0), (16.6, 13.0), (15.0, 14.5)]);
            pen.line(&[(18.2, 7.5), (20.0, 10.0), (20.0, 14.0), (18.2, 16.5)]);
        }
        Icon::Microphone => {
            pen.rect_fill(8.5, 2.5, 7.0, 11.5, 3.5, pen.color);
            pen.line(&[
                (7.0, 10.0),
                (7.0, 11.5),
                (8.0, 13.5),
                (10.0, 15.0),
                (14.0, 15.0),
                (16.0, 13.5),
                (17.0, 11.5),
                (17.0, 10.0),
            ]);
            pen.line(&[(12.0, 15.0), (12.0, 19.0)]);
            pen.line(&[(9.0, 20.0), (15.0, 20.0)]);
        }
        Icon::Cursor => {
            pen.polygon(&[(6.0, 3.0), (18.0, 12.0), (12.5, 13.2)], pen.color);
            pen.polygon(&[(6.0, 3.0), (12.5, 13.2), (9.8, 19.5)], pen.color);
        }
        Icon::Camera => {
            pen.rect_fill(2.0, 6.5, 14.0, 11.0, 2.5, pen.color);
            pen.polygon(
                &[(16.5, 10.5), (22.0, 7.0), (22.0, 17.0), (16.5, 13.5)],
                pen.color,
            );
        }
        Icon::Timer => {
            pen.circle((12.0, 13.0), 7.5);
            pen.line(&[(12.0, 13.0), (12.0, 8.5)]);
            pen.line(&[(12.0, 13.0), (15.0, 14.5)]);
            pen.line(&[(12.0, 3.0), (12.0, 5.5)]);
            pen.line(&[(9.0, 2.5), (15.0, 2.5)]);
        }
        Icon::Hourglass => {
            pen.line(&[(6.0, 3.0), (18.0, 3.0)]);
            pen.line(&[(6.0, 21.0), (18.0, 21.0)]);
            pen.line(&[
                (7.5, 3.0),
                (7.5, 6.5),
                (12.0, 12.0),
                (7.5, 17.5),
                (7.5, 21.0),
            ]);
            pen.line(&[
                (16.5, 3.0),
                (16.5, 6.5),
                (12.0, 12.0),
                (16.5, 17.5),
                (16.5, 21.0),
            ]);
            pen.polygon(&[(9.5, 19.5), (14.5, 19.5), (12.0, 16.0)], pen.color);
        }
        Icon::Folder => {
            pen.polygon(
                &[(2.5, 5.0), (9.0, 5.0), (11.0, 7.5), (2.5, 7.5)],
                pen.color,
            );
            pen.rect_fill(2.5, 7.0, 19.0, 12.5, 1.8, pen.color);
        }
        Icon::Refresh => {
            let center = pen.p(12.0, 12.0);
            let radius = pen.s(7.5);
            let start = -0.35 * PI;
            let end = 1.35 * PI;
            let points: Vec<Pos2> = (0..=32)
                .map(|i| {
                    let angle = start + (end - start) * (i as f32 / 32.0);
                    center + vec2(angle.cos(), angle.sin()) * radius
                })
                .collect();
            painter.line(points, Stroke::new(stroke, color));

            let tip = center + vec2(end.cos(), end.sin()) * radius;
            let tangent = vec2(-end.sin(), end.cos());
            let normal = vec2(tangent.y, -tangent.x);
            let back = tip - tangent * pen.s(3.2);
            painter.line_segment(
                [tip, back + normal * pen.s(2.4)],
                Stroke::new(stroke, color),
            );
            painter.line_segment(
                [tip, back - normal * pen.s(2.4)],
                Stroke::new(stroke, color),
            );
        }
        Icon::Record => {
            pen.circle((12.0, 12.0), 8.0);
            pen.dot((12.0, 12.0), 4.5);
        }
        Icon::Stop => {
            pen.rect_fill(6.0, 6.0, 12.0, 12.0, 2.0, pen.color);
        }
        Icon::Pause => {
            pen.rect_fill(7.0, 5.0, 3.5, 14.0, 1.2, pen.color);
            pen.rect_fill(13.5, 5.0, 3.5, 14.0, 1.2, pen.color);
        }
        Icon::Play => {
            pen.polygon(&[(8.0, 5.0), (19.0, 12.0), (8.0, 19.0)], pen.color);
        }
        Icon::Screenshot => {
            pen.rect(3.0, 7.0, 18.0, 13.0, 2.5);
            pen.circle((12.0, 13.5), 3.5);
            pen.line(&[(8.5, 7.0), (10.0, 4.5), (14.0, 4.5), (15.5, 7.0)]);
            pen.dot((18.0, 10.0), 0.9);
        }
        Icon::Image => {
            pen.rect(3.0, 4.0, 18.0, 16.0, 2.0);
            pen.line(&[
                (5.0, 17.0),
                (9.0, 11.0),
                (12.0, 14.0),
                (15.0, 10.0),
                (19.0, 17.0),
            ]);
            pen.dot((8.0, 8.0), 1.2);
        }
        Icon::Trash => {
            pen.line(&[(4.5, 6.5), (19.5, 6.5)]);
            pen.line(&[(9.0, 6.5), (9.8, 4.0), (14.2, 4.0), (15.0, 6.5)]);
            pen.line(&[(6.5, 6.5), (7.5, 20.0), (16.5, 20.0), (17.5, 6.5)]);
            pen.line(&[(10.0, 10.0), (10.0, 17.0)]);
            pen.line(&[(12.0, 10.0), (12.0, 17.0)]);
            pen.line(&[(14.0, 10.0), (14.0, 17.0)]);
        }
        Icon::Check => {
            pen.line(&[(4.5, 12.5), (9.5, 17.0), (19.5, 6.5)]);
        }
        Icon::Open => {
            pen.line(&[(13.0, 4.0), (20.0, 4.0), (20.0, 11.0)]);
            pen.line(&[(20.0, 4.0), (11.0, 13.0)]);
            pen.line(&[
                (9.0, 6.0),
                (4.0, 6.0),
                (4.0, 20.0),
                (18.0, 20.0),
                (18.0, 15.0),
            ]);
        }
        Icon::Keyboard => {
            pen.rect(2.0, 6.0, 20.0, 12.0, 2.0);
            for x in [5.0, 8.0, 11.0, 14.0, 17.0] {
                pen.line(&[(x, 10.0), (x + 1.0, 10.0)]);
                pen.line(&[(x, 13.0), (x + 1.0, 13.0)]);
            }
            pen.line(&[(8.0, 15.5), (16.0, 15.5)]);
        }
        Icon::Alert => {
            pen.line(&[(12.0, 3.5), (21.0, 20.0), (3.0, 20.0), (12.0, 3.5)]);
            pen.line(&[(12.0, 9.0), (12.0, 14.0)]);
            pen.dot((12.0, 17.0), 0.9);
        }
        Icon::Close => {
            pen.line(&[(6.0, 6.0), (18.0, 18.0)]);
            pen.line(&[(18.0, 6.0), (6.0, 18.0)]);
        }
        Icon::Bolt => {
            pen.polygon(
                &[(13.0, 2.0), (5.0, 13.0), (11.0, 13.0), (13.0, 10.0)],
                pen.color,
            );
            pen.polygon(
                &[(11.0, 13.0), (9.0, 22.0), (19.0, 10.0), (13.0, 10.0)],
                pen.color,
            );
        }
        Icon::Mirror => {
            for (y0, y1) in [(3.0, 6.0), (9.0, 12.0), (15.0, 18.0), (20.0, 21.0)] {
                pen.line(&[(12.0, y0), (12.0, y1)]);
            }
            pen.line(&[(9.0, 6.0), (3.0, 18.0), (9.0, 18.0), (9.0, 6.0)]);
            pen.polygon(&[(15.0, 6.0), (21.0, 18.0), (15.0, 18.0)], pen.color);
        }
        Icon::Corner => {
            pen.rect(3.0, 4.0, 18.0, 16.0, 2.0);
            pen.rect_fill(12.5, 12.0, 6.0, 5.5, 1.2, pen.color);
        }
        Icon::Info => {
            pen.circle((12.0, 12.0), 9.0);
            pen.line(&[(12.0, 11.0), (12.0, 16.5)]);
            pen.dot((12.0, 7.8), 1.1);
        }
        Icon::Copy | Icon::Duplicate => {
            pen.rect(9.0, 9.0, 11.5, 11.5, 2.0);
            pen.line(&[
                (15.0, 5.5),
                (15.0, 4.0),
                (4.0, 4.0),
                (4.0, 15.0),
                (5.5, 15.0),
            ]);
            if icon == Icon::Duplicate {
                pen.line(&[(14.75, 12.0), (14.75, 17.5)]);
                pen.line(&[(12.0, 14.75), (17.5, 14.75)]);
            }
        }
        Icon::Scissors => {
            pen.circle((6.5, 17.5), 2.8);
            pen.circle((17.5, 17.5), 2.8);
            pen.line(&[(8.6, 15.6), (17.0, 3.5)]);
            pen.line(&[(15.4, 15.6), (7.0, 3.5)]);
        }
        Icon::Undo | Icon::Redo => {
            let flip = |x: f32| if icon == Icon::Redo { 24.0 - x } else { x };
            pen.line(&[(flip(9.0), 4.5), (flip(4.5), 9.0), (flip(9.0), 13.5)]);
            let mut curve = vec![(flip(4.5), 9.0)];
            for (x, y) in arc((13.5, 13.5), 4.5, -90.0, 90.0, 12) {
                curve.push((flip(x + 0.0), y));
            }
            curve.push((flip(8.0), 18.0));
            pen.line(&curve);
        }
        Icon::Plus => {
            pen.line(&[(12.0, 5.0), (12.0, 19.0)]);
            pen.line(&[(5.0, 12.0), (19.0, 12.0)]);
        }
        Icon::Minus => {
            pen.line(&[(5.0, 12.0), (19.0, 12.0)]);
        }
        Icon::SkipStart => {
            pen.rect_fill(5.0, 5.0, 2.6, 14.0, 1.0, pen.color);
            pen.polygon(&[(19.0, 5.0), (19.0, 19.0), (8.5, 12.0)], pen.color);
        }
        Icon::SkipEnd => {
            pen.rect_fill(16.4, 5.0, 2.6, 14.0, 1.0, pen.color);
            pen.polygon(&[(5.0, 5.0), (15.5, 12.0), (5.0, 19.0)], pen.color);
        }
        Icon::StepBack => {
            pen.line(&[(14.5, 6.0), (8.5, 12.0), (14.5, 18.0)]);
        }
        Icon::StepForward => {
            pen.line(&[(9.5, 6.0), (15.5, 12.0), (9.5, 18.0)]);
        }
        Icon::Export => {
            pen.line(&[(12.0, 3.5), (12.0, 14.5)]);
            pen.line(&[(7.5, 8.0), (12.0, 3.5), (16.5, 8.0)]);
            pen.line(&[(4.5, 13.5), (4.5, 19.5), (19.5, 19.5), (19.5, 13.5)]);
        }
        Icon::Import => {
            pen.line(&[(12.0, 3.5), (12.0, 14.5)]);
            pen.line(&[(7.5, 10.0), (12.0, 14.5), (16.5, 10.0)]);
            pen.line(&[(4.5, 13.5), (4.5, 19.5), (19.5, 19.5), (19.5, 13.5)]);
        }
        Icon::Mute => {
            pen.rect_fill(3.0, 8.8, 5.5, 6.4, 1.0, pen.color);
            pen.polygon(
                &[(7.5, 8.8), (12.5, 4.5), (12.5, 19.5), (7.5, 15.2)],
                pen.color,
            );
            pen.line(&[(15.5, 9.0), (21.0, 15.0)]);
            pen.line(&[(21.0, 9.0), (15.5, 15.0)]);
        }
        Icon::Fit => {
            pen.line(&[(3.5, 8.0), (3.5, 3.5), (8.0, 3.5)]);
            pen.line(&[(16.0, 3.5), (20.5, 3.5), (20.5, 8.0)]);
            pen.line(&[(3.5, 16.0), (3.5, 20.5), (8.0, 20.5)]);
            pen.line(&[(20.5, 16.0), (20.5, 20.5), (16.0, 20.5)]);
            pen.line(&[(7.5, 12.0), (16.5, 12.0)]);
        }
        Icon::Save => {
            pen.line(&[
                (4.0, 4.0),
                (16.5, 4.0),
                (20.0, 7.5),
                (20.0, 20.0),
                (4.0, 20.0),
                (4.0, 4.0),
            ]);
            pen.rect_fill(7.5, 4.0, 8.0, 5.0, 0.6, pen.color);
            pen.rect(7.5, 13.0, 9.0, 7.0, 1.0);
        }
        Icon::Edit => {
            pen.rect(3.0, 6.0, 18.0, 12.0, 2.0);
            pen.line(&[(3.0, 10.0), (21.0, 10.0)]);
            pen.rect_fill(6.0, 12.5, 6.0, 3.0, 0.8, pen.color);
            pen.line(&[(15.0, 6.0), (15.0, 18.0)]);
        }
    }
}

/// Points on a circle from `start` to `end` degrees (y axis pointing down).
fn arc(center: (f32, f32), radius: f32, start: f32, end: f32, steps: usize) -> Vec<(f32, f32)> {
    (0..=steps)
        .map(|i| {
            let t = start + (end - start) * i as f32 / steps as f32;
            let a = t.to_radians();
            (center.0 + radius * a.cos(), center.1 + radius * a.sin())
        })
        .collect()
}

struct Pen<'a> {
    painter: &'a Painter,
    rect: Rect,
    stroke: f32,
    color: Color32,
}

impl Pen<'_> {
    fn p(&self, x: f32, y: f32) -> Pos2 {
        self.rect.min + vec2(x / 24.0 * self.rect.width(), y / 24.0 * self.rect.height())
    }

    fn s(&self, value: f32) -> f32 {
        value / 24.0 * self.rect.width()
    }

    fn line(&self, points: &[(f32, f32)]) {
        let points: Vec<Pos2> = points.iter().map(|&(x, y)| self.p(x, y)).collect();
        self.painter
            .line(points.clone(), Stroke::new(self.stroke, self.color));
        let radius = self.stroke * 0.5;
        for point in points {
            self.painter.circle_filled(point, radius, self.color);
        }
    }

    fn circle(&self, center: (f32, f32), radius: f32) {
        self.painter.circle_stroke(
            self.p(center.0, center.1),
            self.s(radius),
            Stroke::new(self.stroke, self.color),
        );
    }

    fn dot(&self, center: (f32, f32), radius: f32) {
        self.painter
            .circle_filled(self.p(center.0, center.1), self.s(radius), self.color);
    }

    fn rect(&self, x: f32, y: f32, width: f32, height: f32, radius: f32) {
        self.painter.rect_stroke(
            Rect::from_min_size(self.p(x, y), vec2(self.s(width), self.s(height))),
            CornerRadius::same(self.s(radius).round().clamp(0.0, 255.0) as u8),
            Stroke::new(self.stroke, self.color),
            StrokeKind::Middle,
        );
    }

    fn rect_fill(&self, x: f32, y: f32, width: f32, height: f32, radius: f32, fill: Color32) {
        self.painter.rect_filled(
            Rect::from_min_size(self.p(x, y), vec2(self.s(width), self.s(height))),
            CornerRadius::same(self.s(radius).round().clamp(0.0, 255.0) as u8),
            fill,
        );
    }

    fn polygon(&self, points: &[(f32, f32)], fill: Color32) {
        let points: Vec<Pos2> = points.iter().map(|&(x, y)| self.p(x, y)).collect();
        self.painter
            .add(Shape::convex_polygon(points, fill, Stroke::NONE));
    }
}
