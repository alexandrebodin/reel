use crate::events::{Event, Rect};
use crate::style::Style;

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
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ripple {
    pub x: f64,
    pub y: f64,
    /// 0 when it starts, 1 when it has faded out.
    pub progress: f64,
}

const ZOOM_IN_MS: f64 = 700.0;
const FOCUS_FOLLOWS_MS: f64 = 800.0;
const FIT_MARGIN: f64 = 1.25;
const VIEW_MARGIN: f64 = 24.0;
const HOLD_MS: f64 = 1_200.0;
const UNZOOM_MS: f64 = 600.0;
const MIN_ZOOM_STAY_MS: f64 = 700.0;
const CLUSTER_GAP_MS: f64 = 2_500.0;
const LAST_FRAME_MS: f64 = 200.0;
const CURSOR_TRAVEL_MS: f64 = 650.0;
const MIN_CURSOR_TRAVEL_MS: f64 = 80.0;
const CURSOR_ARRIVES_EARLY_MS: f64 = 120.0;
const PRESS_LEAD_MS: f64 = 60.0;
const PRESS_MS: f64 = 140.0;
const RELEASE_MS: f64 = 80.0;
const RIPPLE_MS: f64 = 500.0;
const IDLE_EDGE_MS: f64 = 400.0;
const IDLE_TARGET_MS: f64 = 600.0;
const MIN_IDLE_SPEED: f64 = 3.0;
const MAX_IDLE_SPEED: f64 = 12.0;
const TYPING_SPEED: f64 = 2.0;
const MIN_FAST_TYPING_MS: f64 = 1_500.0;

pub struct Plan {
    pub frame: Size,
    pub time: TimeMap,
    camera: Vec<(f64, View)>,
    cursor_start: (f64, f64),
    moves: Vec<Move>,
    clicks: Vec<(f64, (f64, f64))>,
}

struct Move {
    start_ms: f64,
    end_ms: f64,
    from: (f64, f64),
    to: (f64, f64),
}

impl Plan {
    pub fn new(events: &[Event], duration_ms: f64, frame: Size, style: &Style) -> Plan {
        let cursor_start = (frame.w * 0.62, frame.h * 0.72);
        let (camera, moves) = choreograph(events, duration_ms, cursor_start, frame, style);
        Plan {
            frame,
            time: TimeMap::new(events, duration_ms),
            camera,
            cursor_start,
            moves,
            clicks: events
                .iter()
                .filter_map(|e| match e {
                    Event::Click { at_ms, target, .. } => Some((*at_ms, target.center())),
                    _ => None,
                })
                .collect(),
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
                return lerp_view(from, to, ease_in_out(p));
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
                position = curve(m.from, m.to, ease_in_out((t_ms - m.start_ms) / (m.end_ms - m.start_ms)));
                break;
            }
            position = m.to;
        }
        let pressed = self.clicks.iter().map(|(at, _)| press_amount(t_ms - at)).fold(0.0, f64::max);
        Cursor { x: position.0, y: position.1, pressed }
    }

    pub fn ripples_at(&self, t_ms: f64) -> Vec<Ripple> {
        self.clicks
            .iter()
            .filter(|(at, _)| t_ms >= *at && t_ms < at + RIPPLE_MS)
            .map(|(at, (x, y))| Ripple { x: *x, y: *y, progress: (t_ms - at) / RIPPLE_MS })
            .collect()
    }
}

fn overview(frame: Size) -> View {
    View { x: 0.0, y: 0.0, w: frame.w, h: frame.h }
}

fn focus_view(target: Rect, frame: Size, max_zoom: f64) -> View {
    let zoom = (frame.w / (target.w * FIT_MARGIN)).min(frame.h / (target.h * FIT_MARGIN)).clamp(1.0, max_zoom);
    let (w, h) = (frame.w / zoom, frame.h / zoom);
    let (cx, cy) = target.center();
    View { x: (cx - w / 2.0).clamp(0.0, frame.w - w), y: (cy - h / 2.0).clamp(0.0, frame.h - h), w, h }
}

fn in_view(view: View, rect: Rect) -> bool {
    rect.x >= view.x + VIEW_MARGIN
        && rect.y >= view.y + VIEW_MARGIN
        && rect.x + rect.w <= view.x + view.w - VIEW_MARGIN
        && rect.y + rect.h <= view.y + view.h - VIEW_MARGIN
}

struct Camera {
    keys: Vec<(f64, View)>,
    shown: View,
}

impl Camera {
    /// Moves to `view` from `start_ms` on, and returns when it gets there.
    fn move_to(&mut self, start_ms: f64, length_ms: f64, view: View) -> f64 {
        let start_ms = start_ms.max(self.keys[self.keys.len() - 1].0);
        self.keys.push((start_ms, self.shown));
        self.keys.push((start_ms + length_ms, view));
        self.shown = view;
        start_ms + length_ms
    }
}

/// Decides the camera and the cursor together, because one rule ties them: the cursor only travels
/// through what the viewer can see, so a target out of view means zooming out first. The camera
/// comes back in once the click has landed, and stays still for anything already in view.
fn choreograph(events: &[Event], duration_ms: f64, cursor_start: (f64, f64), frame: Size, style: &Style) -> (Vec<(f64, View)>, Vec<Move>) {
    let pauses = pauses(events);
    let mut actions: Vec<&Event> = events.iter().filter(|e| e.camera_frame().is_some()).collect();
    actions.sort_by(|a, b| a.at_ms().total_cmp(&b.at_ms()));

    let wide = overview(frame);
    let mut camera = Camera { keys: vec![(0.0, wide)], shown: wide };
    let mut moves = Vec::new();
    let (mut position, mut free_from) = (cursor_start, 0.0);
    for (i, event) in actions.iter().enumerate() {
        let arrive_ms = event.at_ms() - CURSOR_ARRIVES_EARLY_MS;
        let mut travel_ms = (arrive_ms - CURSOR_TRAVEL_MS).max(free_from);
        let target_hidden = event.cursor_target().is_some_and(|t| !in_view(camera.shown, t));
        if camera.shown != wide && (target_hidden || event.at_ms() - free_from > CLUSTER_GAP_MS) {
            let start_ms = (free_from + HOLD_MS).min(travel_ms - UNZOOM_MS).max(free_from);
            travel_ms = travel_ms.max(camera.move_to(start_ms, UNZOOM_MS, wide));
        }
        if let Some(target) = event.cursor_target() {
            let to = target.center();
            if position != to {
                moves.push(Move { start_ms: travel_ms.min(arrive_ms - MIN_CURSOR_TRAVEL_MS), end_ms: arrive_ms, from: position, to });
            }
            position = to;
        }
        let next = actions.get(i + 1).copied();
        free_from = attention_end(event, next, &pauses);

        if let Some(rect) = event.camera_frame() {
            let view = focus_view(rect, frame, style.zoom.level);
            let wanted = camera.shown == wide || !in_view(camera.shown, rect);
            if wanted && !focus_follows(event, next) && stays_long_enough(event.at_ms(), view, next, free_from) {
                camera.move_to(event.at_ms(), ZOOM_IN_MS, view);
            }
        }
    }
    if camera.shown != wide {
        camera.move_to((free_from + HOLD_MS).min(duration_ms - UNZOOM_MS - LAST_FRAME_MS), UNZOOM_MS, wide);
    }
    (camera.keys, moves)
}

/// What a click opened is the thing to look at, so the camera waits for it.
fn focus_follows(event: &Event, next: Option<&Event>) -> bool {
    matches!(next, Some(Event::Focus { at_ms, .. }) if at_ms - event.at_ms() <= FOCUS_FOLLOWS_MS)
}

/// A zoom that has to undo itself straight away for the next target is only noise.
fn stays_long_enough(at_ms: f64, view: View, next: Option<&Event>, free_from: f64) -> bool {
    let Some(next) = next else {
        return true;
    };
    if next.cursor_target().is_none_or(|t| in_view(view, t)) {
        return true;
    }
    let unzoom_ms = (next.at_ms() - CURSOR_ARRIVES_EARLY_MS - CURSOR_TRAVEL_MS - UNZOOM_MS).max(free_from);
    unzoom_ms - (at_ms + ZOOM_IN_MS) >= MIN_ZOOM_STAY_MS
}

fn pauses(events: &[Event]) -> Vec<(f64, f64)> {
    let mut pauses: Vec<(f64, f64)> = events
        .iter()
        .filter_map(|e| match e {
            Event::Pause { at_ms, until_ms } => Some((*at_ms, *until_ms)),
            _ => None,
        })
        .collect();
    pauses.sort_by(|a, b| a.0.total_cmp(&b.0));
    pauses
}

/// When the viewer stops looking at an action: a scripted pause right after it is reading time.
/// A pause after the next action belongs to that action.
fn attention_end(event: &Event, next: Option<&Event>, pauses: &[(f64, f64)]) -> f64 {
    let next_ms = next.map_or(f64::INFINITY, Event::at_ms);
    pauses
        .iter()
        .filter(|(start, _)| *start >= event.at_ms() && *start < next_ms)
        .fold(event.until_ms(), |end, (start, until)| if start - end <= CLUSTER_GAP_MS { end.max(*until) } else { end })
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

/// Maps output time to source time: idle stretches play fast or are cut, typing plays faster.
pub struct TimeMap {
    segments: Vec<Segment>,
    duration_ms: f64,
}

impl TimeMap {
    pub fn new(events: &[Event], duration_ms: f64) -> TimeMap {
        let mut busy: Vec<(f64, f64)> = events
            .iter()
            .map(|e| match e {
                Event::Pause { at_ms, until_ms } => (*at_ms, until_ms + HOLD_MS + UNZOOM_MS),
                _ => (
                    e.at_ms() - UNZOOM_MS - CURSOR_TRAVEL_MS - CURSOR_ARRIVES_EARLY_MS,
                    e.until_ms() + ZOOM_IN_MS + HOLD_MS + UNZOOM_MS,
                ),
            })
            .map(|(a, b)| (a.clamp(0.0, duration_ms), b.clamp(0.0, duration_ms)))
            .collect();
        busy.sort_by(|a, b| a.0.total_cmp(&b.0));

        let mut zones = Vec::new();
        let mut free_from = 0.0_f64;
        for (start, end) in busy.iter().copied().chain(std::iter::once((duration_ms, duration_ms))) {
            if start > free_from {
                zones.extend(idle_zone(free_from, start));
            }
            free_from = free_from.max(end);
        }
        for event in events {
            if let Event::Type { at_ms, until_ms, .. } = event {
                let (start_ms, end_ms) = (at_ms + 250.0, until_ms - 150.0);
                if end_ms - start_ms > MIN_FAST_TYPING_MS {
                    zones.push(Segment { start_ms, end_ms, speed: TYPING_SPEED });
                }
            }
        }
        zones.sort_by(|a, b| a.start_ms.total_cmp(&b.start_ms));

        let mut segments = Vec::new();
        let mut at = 0.0;
        for zone in zones {
            if zone.start_ms > at {
                segments.push(Segment { start_ms: at, end_ms: zone.start_ms, speed: 1.0 });
            }
            segments.push(zone);
            at = zone.end_ms;
        }
        if at < duration_ms {
            segments.push(Segment { start_ms: at, end_ms: duration_ms, speed: 1.0 });
        }
        TimeMap { segments, duration_ms }
    }

    pub fn output_duration_ms(&self) -> f64 {
        self.segments.iter().map(Segment::output_ms).sum()
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

/// Keeps a short real-speed edge on both sides so the jump never looks abrupt.
fn idle_zone(start_ms: f64, end_ms: f64) -> Option<Segment> {
    let (start_ms, end_ms) = (start_ms + IDLE_EDGE_MS, end_ms - IDLE_EDGE_MS);
    let length = end_ms - start_ms;
    if length <= 200.0 {
        return None;
    }
    let speed = length / IDLE_TARGET_MS;
    let speed = if speed > MAX_IDLE_SPEED { f64::INFINITY } else { speed.max(MIN_IDLE_SPEED) };
    Some(Segment { start_ms, end_ms, speed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Zoom;

    const FRAME: Size = Size { w: 1920.0, h: 1080.0 };
    const BUTTON: Rect = Rect { x: 900.0, y: 500.0, w: 100.0, h: 32.0 };
    const ZOOM: f64 = 1.4;

    fn plan(events: &[Event], duration_ms: f64) -> Plan {
        Plan::new(events, duration_ms, FRAME, &Style::default())
    }

    fn click(at_ms: f64, target: Rect) -> Event {
        Event::Click { at_ms, target, frame: target }
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
        let style = Style { zoom: Zoom { level: 1.2 }, ..Style::default() };
        let plan = Plan::new(&[click(2_000.0, BUTTON)], 6_000.0, FRAME, &style);

        assert!(close(plan.camera_at(3_000.0).zoom(FRAME), 1.2), "zoom was {}", plan.camera_at(3_000.0).zoom(FRAME));
    }

    #[test]
    fn never_moves_the_camera_at_a_zoom_level_of_one() {
        let style = Style { zoom: Zoom { level: 1.0 }, ..Style::default() };
        let plan = Plan::new(&[click(2_000.0, BUTTON)], 6_000.0, FRAME, &style);

        for t in [0.0, 2_500.0, 3_000.0, 5_000.0] {
            assert_eq!(plan.camera_at(t), overview(FRAME), "the camera moved at {t}");
        }
    }

    #[test]
    fn keeps_the_row_of_a_clicked_control_in_view() {
        let menu_button = Rect { x: 1425.0, y: 485.0, w: 32.0, h: 32.0 };
        let row = Rect { x: 704.0, y: 472.0, w: 768.0, h: 58.0 };
        let plan = plan(&[Event::Click { at_ms: 2_000.0, target: menu_button, frame: row }], 6_000.0);

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
        let events = [Event::Type { at_ms: 1_000.0, until_ms: 1_900.0, target: field, frame: field }];
        let plan = plan(&events, 4_000.0);

        assert!(close(plan.time.output_duration_ms(), 4_000.0), "output was {}", plan.time.output_duration_ms());
    }

    #[test]
    fn plays_typing_faster() {
        let field = Rect { x: 600.0, y: 400.0, w: 500.0, h: 40.0 };
        let events = [Event::Type { at_ms: 1_000.0, until_ms: 6_000.0, target: field, frame: field }];
        let plan = plan(&events, 9_000.0);

        assert!(plan.time.output_duration_ms() < 8_000.0, "output was {}", plan.time.output_duration_ms());
    }
}
