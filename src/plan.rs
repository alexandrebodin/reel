use crate::events::Pointer;
use crate::style::{Clicks, CursorMotion, CursorStyle, Style, ZoomAnimation};
use crate::timeline::{cursor_start, Move, SpeedSpan, Timeline, ZoomSpan, UNZOOM_MS, ZOOM_IN_MS};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size {
    pub w: f64,
    pub h: f64,
}

/// The part of the source frame the camera shows. Always the frame's aspect ratio.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl View {
    pub fn zoom(&self, frame: Size) -> f64 {
        frame.w / self.w
    }

    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cursor {
    pub x: f64,
    pub y: f64,
    /// 0 when released, 1 while the button is down.
    pub pressed: f64,
    /// 0 when hidden, 1 when fully shown.
    pub opacity: f64,
    pub pointer: Pointer,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ripple {
    pub x: f64,
    pub y: f64,
    /// 0 when it starts, 1 when it has faded out.
    pub progress: f64,
}

const PRESS_LEAD_MS: f64 = 60.0;
const PRESS_MS: f64 = 140.0;
const RELEASE_MS: f64 = 80.0;
const RIPPLE_MS: f64 = 500.0;
const LEAVES_ELEMENT_MS: f64 = 80.0;
const ENTERS_ELEMENT_MS: f64 = 90.0;
const IDLE_BEFORE_HIDING_MS: f64 = 800.0;
const FADE_MS: f64 = 180.0;

pub struct Plan {
    pub frame: Size,
    pub time: TimeMap,
    camera: Vec<(f64, View)>,
    cursor_start: (f64, f64),
    moves: Vec<Travel>,
    clicks: Vec<(f64, (f64, f64))>,
    typing: Vec<(f64, f64)>,
    cursor_style: CursorStyle,
    click_style: Clicks,
    zoom_animation: ZoomAnimation,
}

struct Travel {
    start_ms: f64,
    end_ms: f64,
    from: (f64, f64),
    to: (f64, f64),
    pointer: Pointer,
}

impl Plan {
    pub fn new(timeline: &Timeline, duration_ms: f64, frame: Size, style: &Style) -> Plan {
        let cursor_start = cursor_start(frame);
        Plan {
            frame,
            time: TimeMap::new(&timeline.speeds, duration_ms),
            camera: camera_keys(&timeline.zooms, frame),
            cursor_start,
            moves: travels(&timeline.moves, cursor_start),
            clicks: timeline.clicks.iter().map(|c| (c.at_ms, (c.x, c.y))).collect(),
            typing: timeline.typing.iter().map(|t| (t.start_ms, t.end_ms)).collect(),
            cursor_style: style.cursor.clone(),
            click_style: style.clicks.clone(),
            zoom_animation: style.zoom.animation,
        }
    }

    pub fn camera_at(&self, t_ms: f64) -> View {
        let first = self.camera[0];
        if t_ms <= first.0 {
            return first.1;
        }
        for pair in self.camera.windows(2) {
            let ((t0, from), (t1, to)) = (pair[0], pair[1]);
            if t_ms < t1 {
                if t1 <= t0 {
                    return to;
                }
                let p = (t_ms - t0) / (t1 - t0);
                return match self.zoom_animation {
                    ZoomAnimation::Smooth => lerp_view(from, to, ease_in_out(p)),
                    ZoomAnimation::Dynamic => inside_frame(lerp_view(from, to, ease_out_back(p)), self.frame),
                };
            }
        }
        self.camera[self.camera.len() - 1].1
    }

    pub fn cursor_at(&self, t_ms: f64) -> Cursor {
        let mut position = self.cursor_start;
        for m in &self.moves {
            if t_ms < m.start_ms {
                break;
            }
            if t_ms < m.end_ms {
                let p = (t_ms - m.start_ms) / (m.end_ms - m.start_ms);
                position = match self.cursor_style.motion {
                    CursorMotion::Natural => curve(m.from, m.to, ease_in_out(p)),
                    CursorMotion::Smooth => line(m.from, m.to, ease_sine(p)),
                };
                break;
            }
            position = m.to;
        }
        let pressed = if self.click_style.press { self.clicks.iter().map(|(at, _)| press_amount(t_ms - at)).fold(0.0, f64::max) } else { 0.0 };
        Cursor { x: position.0, y: position.1, pressed, opacity: self.cursor_opacity(t_ms), pointer: self.pointer_at(t_ms) }
    }

    /// The arrow crossing the page, and whatever the element it rests on shows.
    fn pointer_at(&self, t_ms: f64) -> Pointer {
        if !self.cursor_style.adaptive {
            return Pointer::Arrow;
        }
        let mut pointer = Pointer::Arrow;
        for m in &self.moves {
            if t_ms < m.start_ms + LEAVES_ELEMENT_MS {
                break;
            }
            pointer = if t_ms < m.end_ms - ENTERS_ELEMENT_MS { Pointer::Arrow } else { m.pointer };
        }
        pointer
    }

    fn cursor_opacity(&self, t_ms: f64) -> f64 {
        let style = &self.cursor_style;
        let mut opacity: f64 = 1.0;
        if style.hide_when_idle {
            let moving = self.moves.iter().map(|m| (m.start_ms, m.end_ms));
            let clicking = self.clicks.iter().map(|(at, _)| (at - PRESS_LEAD_MS, at + PRESS_MS + RELEASE_MS));
            opacity = moving.chain(clicking).map(|(from, to)| shown(t_ms, from, to + IDLE_BEFORE_HIDING_MS)).fold(0.0, f64::max);
        }
        if style.hide_when_typing {
            let typing = self.typing.iter().map(|(from, to)| shown(t_ms, from + FADE_MS, *to)).fold(0.0, f64::max);
            opacity = opacity.min(1.0 - typing);
        }
        opacity
    }

    pub fn ripples_at(&self, t_ms: f64) -> Vec<Ripple> {
        if !self.click_style.ripple {
            return Vec::new();
        }
        self.clicks
            .iter()
            .filter(|(at, _)| t_ms >= *at && t_ms < at + RIPPLE_MS)
            .map(|(at, (x, y))| Ripple { x: *x, y: *y, progress: (t_ms - at) / RIPPLE_MS })
            .collect()
    }
}

pub fn overview(frame: Size) -> View {
    View { x: 0.0, y: 0.0, w: frame.w, h: frame.h }
}

/// A zoom too short for both moves shares its time between them.
fn camera_keys(zooms: &[ZoomSpan], frame: Size) -> Vec<(f64, View)> {
    let wide = overview(frame);
    let mut keys = vec![(0.0, wide)];
    let mut shown = wide;
    for (i, zoom) in zooms.iter().enumerate() {
        let view = zoom.view(frame);
        let length = zoom.end_ms - zoom.start_ms;
        let pans_on = zooms.get(i + 1).is_some_and(|next| next.start_ms <= zoom.end_ms);
        let moves_ms = if pans_on { ZOOM_IN_MS } else { ZOOM_IN_MS + UNZOOM_MS };
        let fit = (length / moves_ms).min(1.0);
        keys.push((zoom.start_ms, shown));
        keys.push((zoom.start_ms + ZOOM_IN_MS * fit, view));
        shown = view;
        if !pans_on {
            keys.push((zoom.end_ms - UNZOOM_MS * fit, view));
            keys.push((zoom.end_ms, wide));
            shown = wide;
        }
    }
    keys
}

fn travels(moves: &[Move], cursor_start: (f64, f64)) -> Vec<Travel> {
    let mut from = cursor_start;
    moves
        .iter()
        .map(|m| {
            let to = (m.x, m.y);
            let travel = Travel { start_ms: m.start_ms, end_ms: m.end_ms, from, to, pointer: m.pointer };
            from = to;
            travel
        })
        .collect()
}

fn press_amount(since_click_ms: f64) -> f64 {
    let d = since_click_ms;
    if d < -PRESS_LEAD_MS || d > PRESS_MS + RELEASE_MS {
        0.0
    } else if d < 0.0 {
        (d + PRESS_LEAD_MS) / PRESS_LEAD_MS
    } else if d <= PRESS_MS {
        1.0
    } else {
        1.0 - (d - PRESS_MS) / RELEASE_MS
    }
}

/// 1 from `from` to `to`, fading in just before and out just after.
fn shown(t_ms: f64, from: f64, to: f64) -> f64 {
    ((t_ms - (from - FADE_MS)) / FADE_MS).min(((to + FADE_MS) - t_ms) / FADE_MS).clamp(0.0, 1.0)
}

fn ease_sine(p: f64) -> f64 {
    (1.0 - (p.clamp(0.0, 1.0) * std::f64::consts::PI).cos()) / 2.0
}

fn line(from: (f64, f64), to: (f64, f64), p: f64) -> (f64, f64) {
    (from.0 + (to.0 - from.0) * p, from.1 + (to.1 - from.1) * p)
}

fn ease_out_back(p: f64) -> f64 {
    const OVERSHOOT: f64 = 1.2;
    let q = p.clamp(0.0, 1.0) - 1.0;
    1.0 + (OVERSHOOT + 1.0) * q * q * q + OVERSHOOT * q * q
}

/// An overshoot on the way out would show more than the source has.
fn inside_frame(view: View, frame: Size) -> View {
    let (w, h) = (view.w.min(frame.w), view.h.min(frame.h));
    View { x: view.x.clamp(0.0, frame.w - w), y: view.y.clamp(0.0, frame.h - h), w, h }
}

fn ease_in_out(p: f64) -> f64 {
    let p = p.clamp(0.0, 1.0);
    if p < 0.5 {
        4.0 * p * p * p
    } else {
        1.0 - (-2.0 * p + 2.0).powi(3) / 2.0
    }
}

fn lerp_view(a: View, b: View, p: f64) -> View {
    let mix = |x: f64, y: f64| x + (y - x) * p;
    View { x: mix(a.x, b.x), y: mix(a.y, b.y), w: mix(a.w, b.w), h: mix(a.h, b.h) }
}

/// A hand moves in a slight arc, not a straight line.
fn curve(from: (f64, f64), to: (f64, f64), p: f64) -> (f64, f64) {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let distance = (dx * dx + dy * dy).sqrt();
    let bend = 0.12 * distance;
    let control = ((from.0 + to.0) / 2.0 - dy / distance * bend, (from.1 + to.1) / 2.0 + dx / distance * bend);
    let q = 1.0 - p;
    (
        q * q * from.0 + 2.0 * q * p * control.0 + p * p * to.0,
        q * q * from.1 + 2.0 * q * p * control.1 + p * p * to.1,
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Segment {
    start_ms: f64,
    end_ms: f64,
    /// Infinite for a cut.
    speed: f64,
}

impl Segment {
    fn output_ms(&self) -> f64 {
        if self.speed.is_infinite() {
            0.0
        } else {
            (self.end_ms - self.start_ms) / self.speed
        }
    }
}

/// Maps output time to source time.
pub struct TimeMap {
    segments: Vec<Segment>,
    duration_ms: f64,
}

impl TimeMap {
    pub fn new(speeds: &[SpeedSpan], duration_ms: f64) -> TimeMap {
        let mut segments = Vec::new();
        let mut at = 0.0;
        for span in speeds {
            if span.start_ms > at {
                segments.push(Segment { start_ms: at, end_ms: span.start_ms, speed: 1.0 });
            }
            segments.push(Segment { start_ms: span.start_ms, end_ms: span.end_ms, speed: span.speed.unwrap_or(f64::INFINITY) });
            at = span.end_ms;
        }
        if at < duration_ms {
            segments.push(Segment { start_ms: at, end_ms: duration_ms, speed: 1.0 });
        }
        TimeMap { segments, duration_ms }
    }

    pub fn output_duration_ms(&self) -> f64 {
        self.segments.iter().map(Segment::output_ms).sum()
    }

    /// Where a source time lands in the output. A source time inside a cut lands just after it.
    pub fn output_ms(&self, source_ms: f64) -> f64 {
        let mut elapsed = 0.0;
        for segment in &self.segments {
            if source_ms < segment.end_ms {
                return elapsed + (source_ms - segment.start_ms).max(0.0) / segment.speed;
            }
            elapsed += segment.output_ms();
        }
        elapsed
    }

    pub fn source_ms(&self, output_ms: f64) -> f64 {
        let mut elapsed = 0.0;
        for segment in &self.segments {
            let length = segment.output_ms();
            if output_ms < elapsed + length {
                return segment.start_ms + (output_ms - elapsed) * segment.speed;
            }
            elapsed += length;
        }
        self.duration_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Event, Rect};
    use crate::style::Zoom;

    const FRAME: Size = Size { w: 1920.0, h: 1080.0 };
    const BUTTON: Rect = Rect { x: 900.0, y: 500.0, w: 100.0, h: 32.0 };
    const ZOOM: f64 = 1.4;

    fn auto(events: &[Event], duration_ms: f64, frame: Size, style: &Style) -> Plan {
        Plan::new(&Timeline::auto(events, duration_ms, frame, style), duration_ms, frame, style)
    }

    fn plan(events: &[Event], duration_ms: f64) -> Plan {
        auto(events, duration_ms, FRAME, &Style::default())
    }

    fn click(at_ms: f64, target: Rect) -> Event {
        Event::Click { at_ms, target, frame: target, pointer: Pointer::Arrow }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.5
    }

    #[test]
    fn zooms_onto_a_clicked_button_once_the_click_lands() {
        let plan = plan(&[click(2_000.0, BUTTON)], 6_000.0);

        assert_eq!(plan.camera_at(1_900.0).zoom(FRAME), 1.0, "zoomed in before the click");
        let view = plan.camera_at(3_000.0);
        assert!(close(view.zoom(FRAME), ZOOM), "zoom was {}", view.zoom(FRAME));
        let (cx, cy) = view.center();
        assert!(close(cx, 950.0) && close(cy, 516.0), "centered on ({cx}, {cy})");
    }

    #[test]
    fn zooms_as_far_as_the_style_asks() {
        let style = Style { zoom: Zoom { level: 1.2, ..Zoom::default() }, ..Style::default() };
        let plan = auto(&[click(2_000.0, BUTTON)], 6_000.0, FRAME, &style);

        assert!(close(plan.camera_at(3_000.0).zoom(FRAME), 1.2), "zoom was {}", plan.camera_at(3_000.0).zoom(FRAME));
    }

    #[test]
    fn never_moves_the_camera_at_a_zoom_level_of_one() {
        let style = Style { zoom: Zoom { level: 1.0, ..Zoom::default() }, ..Style::default() };
        let plan = auto(&[click(2_000.0, BUTTON)], 6_000.0, FRAME, &style);

        for t in [0.0, 2_500.0, 3_000.0, 5_000.0] {
            assert_eq!(plan.camera_at(t), overview(FRAME), "the camera moved at {t}");
        }
    }

    fn dynamic(events: &[Event], duration_ms: f64) -> Plan {
        let zoom = Zoom { animation: ZoomAnimation::Dynamic, ..Zoom::default() };
        auto(events, duration_ms, FRAME, &Style { zoom, ..Style::default() })
    }

    #[test]
    fn darts_ahead_of_a_smooth_zoom_and_settles_on_the_same_view() {
        let events = [click(2_000.0, BUTTON)];
        let (smooth, dynamic) = (plan(&events, 6_000.0), dynamic(&events, 6_000.0));

        assert!(dynamic.camera_at(2_150.0).zoom(FRAME) > smooth.camera_at(2_150.0).zoom(FRAME) + 0.05);
        let peak = (0..70).map(|i| dynamic.camera_at(2_000.0 + i as f64 * 10.0).zoom(FRAME)).fold(0.0, f64::max);
        assert!(peak > ZOOM, "never overshot: {peak}");
        assert!(close(dynamic.camera_at(3_000.0).zoom(FRAME), ZOOM));
    }

    #[test]
    fn keeps_a_dynamic_view_inside_the_frame_on_every_tick() {
        let corner = Rect { x: 1880.0, y: 4.0, w: 32.0, h: 32.0 };
        let plan = dynamic(&[click(2_000.0, corner)], 6_000.0);

        for i in 0..600 {
            let view = plan.camera_at(i as f64 * 10.0);
            assert!(view.w <= FRAME.w && view.x >= 0.0 && view.y >= 0.0, "{view:?}");
            assert!(view.x + view.w <= FRAME.w + 0.01 && view.y + view.h <= FRAME.h + 0.01, "{view:?}");
        }
    }

    #[test]
    fn keeps_the_row_of_a_clicked_control_in_view() {
        let menu_button = Rect { x: 1425.0, y: 485.0, w: 32.0, h: 32.0 };
        let row = Rect { x: 704.0, y: 472.0, w: 768.0, h: 58.0 };
        let plan = plan(&[Event::Click { at_ms: 2_000.0, target: menu_button, frame: row, pointer: Pointer::Arrow }], 6_000.0);

        let view = plan.camera_at(3_000.0);

        assert!(view.zoom(FRAME) > 1.3);
        assert!(view.x <= row.x && view.x + view.w >= row.x + row.w, "row cut off by {view:?}");
        let (cx, _) = view.center();
        assert!(close(cx, 1_088.0), "centered on {cx}, not the row");
        assert!(close(plan.cursor_at(2_000.0).x, 1_441.0), "the cursor still goes to the button");
    }

    #[test]
    fn fits_a_dialog_that_opens_after_a_click_without_moving_the_cursor() {
        let dialog = Rect { x: 1_250.0, y: 120.0, w: 520.0, h: 480.0 };
        let events = [click(2_000.0, BUTTON), Event::Focus { at_ms: 2_350.0, frame: dialog }];
        let plan = plan(&events, 8_000.0);

        let view = plan.camera_at(3_200.0);

        assert!(view.zoom(FRAME) > 1.3, "zoom was {}", view.zoom(FRAME));
        assert!(view.y <= dialog.y && view.y + view.h >= dialog.y + dialog.h, "dialog cut off by {view:?}");
        assert!(view.x <= dialog.x && view.x + view.w >= dialog.x + dialog.w, "dialog cut off by {view:?}");
        assert!(close(plan.cursor_at(3_200.0).x, 950.0), "the cursor moved to the dialog");
        assert_eq!(plan.camera_at(2_300.0).zoom(FRAME), 1.0, "the camera moved before the dialog appeared");
    }

    #[test]
    fn holds_the_view_through_a_scripted_pause() {
        let near = Rect { x: 600.0, y: 400.0, w: 100.0, h: 32.0 };
        let events = [click(2_000.0, BUTTON), Event::Pause { at_ms: 2_100.0, until_ms: 7_100.0 }, click(8_000.0, near)];
        let plan = plan(&events, 14_000.0);

        for t in [3_000.0, 5_000.0, 7_000.0] {
            assert!(close(plan.camera_at(t).zoom(FRAME), ZOOM), "zoomed out at {t}");
            assert!(close(plan.camera_at(t).center().0, 950.0), "panned away at {t}");
        }
    }

    #[test]
    fn holds_the_view_through_a_pause_after_the_last_action() {
        let events = [click(2_000.0, BUTTON), Event::Pause { at_ms: 2_100.0, until_ms: 7_100.0 }];
        let plan = plan(&events, 12_000.0);

        assert!(close(plan.camera_at(6_500.0).zoom(FRAME), ZOOM), "zoomed out during the pause");
        assert_eq!(plan.camera_at(11_000.0).zoom(FRAME), 1.0);
    }

    #[test]
    fn keeps_the_view_inside_the_frame_for_a_target_in_a_corner() {
        let corner = Rect { x: 1880.0, y: 4.0, w: 32.0, h: 32.0 };
        let plan = plan(&[click(2_000.0, corner)], 6_000.0);

        let view = plan.camera_at(3_000.0);

        assert!(view.zoom(FRAME) > 1.3);
        assert!(view.x >= 0.0 && view.y >= 0.0);
        assert!(view.x + view.w <= FRAME.w + 0.01 && view.y + view.h <= FRAME.h + 0.01);
    }

    #[test]
    fn shows_the_whole_frame_before_and_well_after_the_action() {
        let plan = plan(&[click(2_000.0, BUTTON)], 10_000.0);

        assert_eq!(plan.camera_at(0.0).zoom(FRAME), 1.0);
        assert_eq!(plan.camera_at(9_000.0).zoom(FRAME), 1.0);
    }

    #[test]
    fn ends_on_the_whole_frame_even_right_after_the_last_action() {
        let plan = plan(&[click(2_000.0, BUTTON)], 3_600.0);

        assert_eq!(plan.camera_at(3_600.0).zoom(FRAME), 1.0);
    }

    #[test]
    fn keeps_the_camera_still_while_the_cursor_moves_inside_the_view() {
        let near = Rect { x: 600.0, y: 400.0, w: 100.0, h: 32.0 };
        let plan = plan(&[click(2_000.0, BUTTON), click(3_500.0, near)], 8_000.0);

        let settled = plan.camera_at(2_800.0);
        for t in [3_000.0, 3_300.0, 3_500.0, 4_000.0] {
            assert_eq!(plan.camera_at(t), settled, "the camera moved at {t}");
        }
        assert!(close(plan.cursor_at(3_500.0).x, 650.0));
    }

    #[test]
    fn zooms_out_before_the_cursor_travels_to_a_target_out_of_view() {
        let far = Rect { x: 40.0, y: 20.0, w: 100.0, h: 32.0 };
        let events = [click(2_000.0, BUTTON), Event::Pause { at_ms: 2_100.0, until_ms: 4_000.0 }, click(5_000.0, far)];
        let plan = plan(&events, 10_000.0);

        assert!(close(plan.camera_at(3_900.0).zoom(FRAME), ZOOM), "zoomed out during the pause");
        let wide_from = (0..=500).map(|i| 4_000.0 + i as f64 * 10.0).find(|t| plan.camera_at(*t).zoom(FRAME) == 1.0);
        let wide_from = wide_from.expect("never zoomed out before the click");
        assert!(close(plan.cursor_at(wide_from - 10.0).x, 950.0), "the cursor left while still zoomed in");
        assert!(plan.cursor_at(wide_from + 200.0).x < 950.0, "the cursor did not travel once zoomed out");
        assert_eq!(plan.camera_at(5_000.0).zoom(FRAME), 1.0, "zoomed in before the click");
        assert!(close(plan.camera_at(6_000.0).zoom(FRAME), ZOOM), "did not zoom onto the new target");
    }

    #[test]
    fn zooms_out_after_a_long_gap_before_a_target_in_view() {
        let near = Rect { x: 600.0, y: 400.0, w: 100.0, h: 32.0 };
        let plan = plan(&[click(2_000.0, BUTTON), click(9_000.0, near)], 14_000.0);

        assert_eq!(plan.camera_at(6_000.0).zoom(FRAME), 1.0);
    }

    #[test]
    fn does_not_zoom_in_for_a_moment_when_the_next_target_is_out_of_view() {
        let far = Rect { x: 40.0, y: 20.0, w: 100.0, h: 32.0 };
        let plan = plan(&[click(2_000.0, BUTTON), click(3_300.0, far)], 8_000.0);

        for t in [2_000.0, 2_400.0, 2_800.0, 3_300.0] {
            assert_eq!(plan.camera_at(t).zoom(FRAME), 1.0, "zoomed in at {t}");
        }
    }

    #[test]
    fn moves_the_cursor_onto_the_target_before_clicking() {
        let plan = plan(&[click(2_000.0, BUTTON)], 6_000.0);

        let at_click = plan.cursor_at(2_000.0);
        assert!(close(at_click.x, 950.0) && close(at_click.y, 516.0));
        let start = plan.cursor_at(0.0);
        let travelling = plan.cursor_at(1_600.0);
        assert!(travelling != start && (travelling.x, travelling.y) != (at_click.x, at_click.y));
    }

    #[test]
    fn gives_the_cursor_its_full_travel_when_a_click_comes_right_after_a_pause() {
        let far = Rect { x: 40.0, y: 20.0, w: 100.0, h: 32.0 };
        let events = [click(2_000.0, BUTTON), Event::Pause { at_ms: 2_100.0, until_ms: 6_000.0 }, click(6_040.0, far)];
        let plan = plan(&events, 12_000.0);

        let arrives = 6_040.0 - 120.0;
        let early = plan.cursor_at(arrives - 300.0).x;
        let late = plan.cursor_at(arrives - 100.0).x;
        assert!(early < 900.0, "had not left 300 ms before arriving: {early}");
        assert!(early > 100.0 && late < early, "not travelling: {early} then {late}");
        assert!(close(plan.cursor_at(arrives - 900.0).x, 950.0), "left before it needed to");
    }

    #[test]
    fn still_holds_the_view_for_most_of_a_pause_before_a_far_target() {
        let far = Rect { x: 40.0, y: 20.0, w: 100.0, h: 32.0 };
        let events = [click(2_000.0, BUTTON), Event::Pause { at_ms: 2_100.0, until_ms: 6_000.0 }, click(6_040.0, far)];
        let plan = plan(&events, 12_000.0);

        assert!(close(plan.camera_at(4_500.0).zoom(FRAME), ZOOM), "zoomed out too early");
        assert_eq!(plan.camera_at(6_040.0 - 120.0 - 650.0).zoom(FRAME), 1.0, "still zoomed in when the cursor set off");
    }

    #[test]
    fn has_the_cursor_on_the_target_at_every_click() {
        let a = Rect { x: 900.0, y: 500.0, w: 100.0, h: 32.0 };
        let b = Rect { x: 1_200.0, y: 760.0, w: 60.0, h: 32.0 };
        let far = Rect { x: 40.0, y: 20.0, w: 100.0, h: 32.0 };
        let timelines = [
            vec![click(2_000.0, a), click(3_300.0, b), Event::Pause { at_ms: 3_400.0, until_ms: 5_000.0 }],
            vec![click(2_000.0, a), Event::Pause { at_ms: 2_100.0, until_ms: 4_000.0 }, click(4_050.0, far)],
            vec![click(2_000.0, a), click(2_300.0, far), click(2_600.0, b)],
        ];

        for events in timelines {
            let plan = plan(&events, 9_000.0);
            for event in &events {
                if let Event::Click { at_ms, target, .. } = event {
                    let (x, y) = target.center();
                    let cursor = plan.cursor_at(*at_ms);
                    assert!(close(cursor.x, x) && close(cursor.y, y), "{cursor:?} at the click on {target:?} in {events:?}");
                }
            }
        }
    }

    fn with_cursor(cursor: CursorStyle, events: &[Event], duration_ms: f64) -> Plan {
        auto(events, duration_ms, FRAME, &Style { cursor, ..Style::default() })
    }

    #[test]
    fn moves_in_an_arc_when_natural_and_a_straight_line_when_smooth() {
        let events = [click(2_000.0, BUTTON)];
        let (start, end) = ((FRAME.w * 0.62, FRAME.h * 0.72), BUTTON.center());
        let off_the_line = |plan: &Plan, t| {
            let cursor = plan.cursor_at(t);
            let (dx, dy) = (end.0 - start.0, end.1 - start.1);
            ((cursor.x - start.0) * dy - (cursor.y - start.1) * dx).abs() / (dx * dx + dy * dy).sqrt()
        };

        let smooth = with_cursor(CursorStyle { motion: CursorMotion::Smooth, ..CursorStyle::default() }, &events, 6_000.0);

        assert!(off_the_line(&plan(&events, 6_000.0), 1_550.0) > 5.0);
        assert!(off_the_line(&smooth, 1_550.0) < 0.01);
        assert!(close(smooth.cursor_at(2_000.0).x, end.0) && close(smooth.cursor_at(2_000.0).y, end.1));
    }

    #[test]
    fn stays_visible_unless_asked_to_hide() {
        let field = Rect { x: 600.0, y: 400.0, w: 500.0, h: 40.0 };
        let events = [Event::Type { at_ms: 1_000.0, until_ms: 3_000.0, target: field, frame: field, pointer: Pointer::Arrow }];
        let plan = plan(&events, 20_000.0);

        for t in [0.0, 2_000.0, 15_000.0] {
            assert_eq!(plan.cursor_at(t).opacity, 1.0);
        }
    }

    #[test]
    fn hides_when_idle_and_returns_before_the_next_move() {
        let events = [click(2_000.0, BUTTON), click(9_000.0, Rect { x: 40.0, y: 20.0, w: 100.0, h: 32.0 })];
        let plan = with_cursor(CursorStyle { hide_when_idle: true, ..CursorStyle::default() }, &events, 12_000.0);

        assert_eq!(plan.cursor_at(0.0).opacity, 0.0, "shown before anything moved");
        assert_eq!(plan.cursor_at(1_700.0).opacity, 1.0, "hidden while moving");
        assert_eq!(plan.cursor_at(2_000.0).opacity, 1.0, "hidden at the click");
        assert_eq!(plan.cursor_at(2_600.0).opacity, 1.0, "hidden right after the click");
        assert_eq!(plan.cursor_at(5_000.0).opacity, 0.0, "shown while idle");
        assert_eq!(plan.cursor_at(8_500.0).opacity, 1.0, "hidden while moving to the next target");
        assert_eq!(plan.cursor_at(9_000.0).opacity, 1.0, "hidden at the second click");
        assert_eq!(plan.cursor_at(11_900.0).opacity, 0.0, "shown after the last click");
    }

    #[test]
    fn shows_a_click_on_the_spot_it_is_already_on_when_hiding_idle() {
        let events = [click(2_000.0, BUTTON), click(9_000.0, BUTTON)];
        let plan = with_cursor(CursorStyle { hide_when_idle: true, ..CursorStyle::default() }, &events, 12_000.0);

        assert_eq!(plan.cursor_at(6_000.0).opacity, 0.0);
        assert_eq!(plan.cursor_at(9_000.0).opacity, 1.0);
    }

    #[test]
    fn hides_while_typing_and_returns_after() {
        let field = Rect { x: 600.0, y: 400.0, w: 500.0, h: 40.0 };
        let events = [Event::Type { at_ms: 2_000.0, until_ms: 4_000.0, target: field, frame: field, pointer: Pointer::Arrow }];
        let plan = with_cursor(CursorStyle { hide_when_typing: true, ..CursorStyle::default() }, &events, 8_000.0);

        assert_eq!(plan.cursor_at(1_500.0).opacity, 1.0);
        assert_eq!(plan.cursor_at(2_000.0).opacity, 1.0, "hidden as it arrived at the field");
        assert_eq!(plan.cursor_at(3_000.0).opacity, 0.0);
        assert_eq!(plan.cursor_at(5_000.0).opacity, 1.0);
    }

    #[test]
    fn shows_what_the_page_would_show_while_resting_on_an_element() {
        let link = Event::Click { at_ms: 2_000.0, target: BUTTON, frame: BUTTON, pointer: Pointer::Hand };
        let far = Rect { x: 40.0, y: 20.0, w: 100.0, h: 32.0 };
        let field = Event::Click { at_ms: 5_000.0, target: far, frame: far, pointer: Pointer::Text };
        let plan = plan(&[link, field], 8_000.0);

        assert_eq!(plan.cursor_at(1_500.0).pointer, Pointer::Arrow, "changed long before arriving");
        assert_eq!(plan.cursor_at(2_000.0).pointer, Pointer::Hand);
        assert_eq!(plan.cursor_at(3_000.0).pointer, Pointer::Hand);
        assert_eq!(plan.cursor_at(4_500.0).pointer, Pointer::Arrow, "kept the hand while crossing the page");
        assert_eq!(plan.cursor_at(5_000.0).pointer, Pointer::Text);
    }

    #[test]
    fn stays_an_arrow_when_not_adaptive() {
        let link = Event::Click { at_ms: 2_000.0, target: BUTTON, frame: BUTTON, pointer: Pointer::Hand };
        let plan = with_cursor(CursorStyle { adaptive: false, ..CursorStyle::default() }, &[link], 6_000.0);

        assert_eq!(plan.cursor_at(2_000.0).pointer, Pointer::Arrow);
    }

    #[test]
    fn presses_the_cursor_and_ripples_on_a_click() {
        let plan = plan(&[click(2_000.0, BUTTON)], 6_000.0);

        assert_eq!(plan.cursor_at(2_050.0).pressed, 1.0);
        assert_eq!(plan.cursor_at(3_000.0).pressed, 0.0);
        let ripples = plan.ripples_at(2_200.0);
        assert_eq!(ripples.len(), 1);
        assert!(ripples[0].progress > 0.0 && ripples[0].progress < 1.0);
        assert!(plan.ripples_at(4_000.0).is_empty());
    }

    #[test]
    fn leaves_out_the_press_and_the_ripple_when_turned_off() {
        let clicks = Clicks { press: false, ripple: false, ..Clicks::default() };
        let plan = auto(&[click(2_000.0, BUTTON)], 6_000.0, FRAME, &Style { clicks, ..Style::default() });

        assert_eq!(plan.cursor_at(2_050.0).pressed, 0.0);
        assert!(plan.ripples_at(2_200.0).is_empty());
    }

    fn zoom_span(start_ms: f64, end_ms: f64, x: f64) -> ZoomSpan {
        ZoomSpan { start_ms, end_ms, x, y: 540.0, level: 2.0 }
    }

    fn from_zooms(zooms: Vec<ZoomSpan>) -> Plan {
        Plan::new(&Timeline { zooms, ..Timeline::default() }, 20_000.0, FRAME, &Style::default())
    }

    #[test]
    fn holds_a_saved_zoom_and_comes_back_by_its_end() {
        let plan = from_zooms(vec![zoom_span(1_000.0, 5_000.0, 500.0)]);

        assert_eq!(plan.camera_at(999.0).zoom(FRAME), 1.0);
        assert!(close(plan.camera_at(1_700.0).zoom(FRAME), 2.0));
        assert!(close(plan.camera_at(4_400.0).zoom(FRAME), 2.0));
        assert!(close(plan.camera_at(3_000.0).center().0, 500.0));
        assert_eq!(plan.camera_at(5_000.0).zoom(FRAME), 1.0);
    }

    #[test]
    fn pans_between_zooms_that_touch_and_zooms_out_between_those_that_do_not() {
        let touching = from_zooms(vec![zoom_span(1_000.0, 4_000.0, 500.0), zoom_span(4_000.0, 8_000.0, 1_400.0)]);
        let apart = from_zooms(vec![zoom_span(1_000.0, 4_000.0, 500.0), zoom_span(4_001.0, 8_000.0, 1_400.0)]);

        assert!(close(touching.camera_at(4_000.0).zoom(FRAME), 2.0));
        assert!(close(touching.camera_at(4_350.0).zoom(FRAME), 2.0), "zoomed out while panning");
        assert!(close(touching.camera_at(5_000.0).center().0, 1_400.0));
        assert_eq!(apart.camera_at(4_000.0).zoom(FRAME), 1.0);
    }

    #[test]
    fn shares_a_short_zooms_time_between_going_in_and_coming_back() {
        let plan = from_zooms(vec![zoom_span(1_000.0, 1_130.0, 500.0)]);

        let peak_at = 1_000.0 + 130.0 * ZOOM_IN_MS / (ZOOM_IN_MS + UNZOOM_MS);
        assert!(close(plan.camera_at(peak_at).zoom(FRAME), 2.0), "{}", plan.camera_at(peak_at).zoom(FRAME));
        assert_eq!(plan.camera_at(1_130.0).zoom(FRAME), 1.0);
    }

    #[test]
    fn stays_on_the_spot_a_saved_zoom_asks_for_even_near_an_edge() {
        let plan = from_zooms(vec![zoom_span(1_000.0, 5_000.0, 10.0)]);

        assert_eq!(plan.camera_at(3_000.0).x, 0.0);
    }

    #[test]
    fn starts_each_saved_move_from_where_the_last_one_ended() {
        let m = |start_ms, end_ms, x| Move { start_ms, end_ms, x, y: 300.0, pointer: Pointer::Arrow };
        let timeline = Timeline { moves: vec![m(1_000.0, 2_000.0, 400.0), m(3_000.0, 4_000.0, 800.0)], ..Timeline::default() };
        let plan = Plan::new(&timeline, 6_000.0, FRAME, &Style::default());

        assert!(close(plan.cursor_at(0.0).x, FRAME.w * 0.62));
        assert!(close(plan.cursor_at(2_500.0).x, 400.0));
        let midway = plan.cursor_at(3_500.0).x;
        assert!(midway > 400.0 && midway < 800.0, "{midway}");
        assert!(close(plan.cursor_at(5_000.0).x, 800.0));
    }

    #[test]
    fn plays_a_saved_speed_change_and_cuts_a_saved_cut() {
        let speeds = vec![SpeedSpan { start_ms: 1_000.0, end_ms: 3_000.0, speed: Some(2.0) }, SpeedSpan { start_ms: 4_000.0, end_ms: 6_000.0, speed: None }];
        let plan = Plan::new(&Timeline { speeds, ..Timeline::default() }, 8_000.0, FRAME, &Style::default());

        assert!(close(plan.time.output_duration_ms(), 5_000.0));
        assert!(close(plan.time.source_ms(1_500.0), 2_000.0));
        assert!(close(plan.time.source_ms(3_000.0), 6_000.0));
    }

    #[test]
    fn finds_the_output_time_of_a_source_time() {
        let speeds = vec![SpeedSpan { start_ms: 1_000.0, end_ms: 3_000.0, speed: Some(2.0) }, SpeedSpan { start_ms: 4_000.0, end_ms: 6_000.0, speed: None }];
        let time = TimeMap::new(&speeds, 8_000.0);

        assert!(close(time.output_ms(500.0), 500.0));
        assert!(close(time.output_ms(2_000.0), 1_500.0));
        assert!(close(time.output_ms(5_000.0), 3_000.0), "inside a cut, the frame after it");
        assert!(close(time.output_ms(7_000.0), 4_000.0));
        assert!(close(time.output_ms(9_000.0), 5_000.0));
    }

    #[test]
    fn plays_a_busy_timeline_at_real_speed() {
        let events: Vec<Event> = (0..6).map(|i| click(500.0 + i as f64 * 1_000.0, BUTTON)).collect();
        let plan = plan(&events, 6_000.0);

        assert!(close(plan.time.output_duration_ms(), 6_000.0));
        assert!(close(plan.time.source_ms(3_000.0), 3_000.0));
    }

    #[test]
    fn fast_forwards_an_idle_stretch() {
        let plan = plan(&[click(1_000.0, BUTTON), click(9_000.0, BUTTON)], 12_000.0);

        let output = plan.time.output_duration_ms();
        assert!(output < 10_000.0, "output was {output}");
        let samples: Vec<f64> = (0..=40).map(|i| plan.time.source_ms(output * i as f64 / 40.0)).collect();
        assert!(samples.windows(2).all(|w| w[1] >= w[0]), "source time went backwards");
        assert!(close(plan.time.source_ms(0.0), 0.0));
        assert!(close(plan.time.source_ms(output), 12_000.0));
    }

    #[test]
    fn cuts_an_idle_stretch_too_long_to_fast_forward() {
        let plan = plan(&[click(1_000.0, BUTTON), click(120_000.0, BUTTON)], 124_000.0);

        assert!(plan.time.output_duration_ms() < 15_000.0, "output was {}", plan.time.output_duration_ms());
    }

    #[test]
    fn keeps_a_scripted_pause_at_real_speed() {
        let events = [click(1_000.0, BUTTON), Event::Pause { at_ms: 1_000.0, until_ms: 9_000.0 }];
        let plan = plan(&events, 9_000.0);

        assert!(close(plan.time.output_duration_ms(), 9_000.0));
    }

    #[test]
    fn keeps_short_typing_at_real_speed() {
        let field = Rect { x: 600.0, y: 400.0, w: 500.0, h: 40.0 };
        let events = [Event::Type { at_ms: 1_000.0, until_ms: 1_900.0, target: field, frame: field, pointer: Pointer::Arrow }];
        let plan = plan(&events, 4_000.0);

        assert!(close(plan.time.output_duration_ms(), 4_000.0), "output was {}", plan.time.output_duration_ms());
    }

    #[test]
    fn plays_typing_faster() {
        let field = Rect { x: 600.0, y: 400.0, w: 500.0, h: 40.0 };
        let events = [Event::Type { at_ms: 1_000.0, until_ms: 6_000.0, target: field, frame: field, pointer: Pointer::Arrow }];
        let plan = plan(&events, 9_000.0);

        assert!(plan.time.output_duration_ms() < 8_000.0, "output was {}", plan.time.output_duration_ms());
    }
}
