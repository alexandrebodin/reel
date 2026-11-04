use crate::events::{Event, Rect};

pub const MAX_ZOOM: f64 = 1.8;

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

const ZOOM_IN_LEAD_MS: f64 = 900.0;
const SETTLE_MS: f64 = 150.0;
const PAN_LEAD_MS: f64 = 650.0;
const HOLD_MS: f64 = 1_200.0;
const ZOOM_OUT_MS: f64 = 1_000.0;
const CLUSTER_GAP_MS: f64 = 2_500.0;
const CURSOR_TRAVEL_MS: f64 = 650.0;
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
    pub fn new(events: &[Event], duration_ms: f64, frame: Size) -> Plan {
        let mut focus: Vec<(&Event, Rect)> = events.iter().filter_map(|e| e.target().map(|r| (e, r))).collect();
        focus.sort_by(|a, b| a.0.at_ms().total_cmp(&b.0.at_ms()));
        let cursor_start = (frame.w * 0.62, frame.h * 0.72);
        Plan {
            frame,
            time: TimeMap::new(events, duration_ms),
            camera: camera_keys(&focus, frame),
            cursor_start,
            moves: cursor_moves(&focus, cursor_start),
            clicks: events
                .iter()
                .filter_map(|e| match e {
                    Event::Click { at_ms, target } => Some((*at_ms, target.center())),
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
                return lerp_view(from, to, ease_in_out((t_ms - t0) / (t1 - t0)));
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

fn focus_view(target: Rect, frame: Size) -> View {
    let zoom = (frame.w / (target.w * 2.5)).min(frame.h / (target.h * 8.0)).clamp(1.0, MAX_ZOOM);
    let (w, h) = (frame.w / zoom, frame.h / zoom);
    let (cx, cy) = target.center();
    View { x: (cx - w / 2.0).clamp(0.0, frame.w - w), y: (cy - h / 2.0).clamp(0.0, frame.h - h), w, h }
}

fn camera_keys(focus: &[(&Event, Rect)], frame: Size) -> Vec<(f64, View)> {
    let mut keys = vec![(0.0, overview(frame))];
    for cluster in clusters(focus) {
        let (first, first_target) = cluster[0];
        let current = keys[keys.len() - 1].1;
        push_key(&mut keys, first.at_ms() - ZOOM_IN_LEAD_MS, current);
        push_key(&mut keys, first.at_ms() - SETTLE_MS, focus_view(first_target, frame));
        for pair in cluster.windows(2) {
            let ((_, previous), (next, next_target)) = (pair[0], pair[1]);
            push_key(&mut keys, next.at_ms() - PAN_LEAD_MS, focus_view(previous, frame));
            push_key(&mut keys, next.at_ms() - SETTLE_MS, focus_view(next_target, frame));
        }
        let (last, last_target) = cluster[cluster.len() - 1];
        push_key(&mut keys, last.until_ms() + HOLD_MS, focus_view(last_target, frame));
        push_key(&mut keys, last.until_ms() + HOLD_MS + ZOOM_OUT_MS, overview(frame));
    }
    keys
}

fn clusters<'a>(focus: &'a [(&'a Event, Rect)]) -> Vec<&'a [(&'a Event, Rect)]> {
    let mut clusters = Vec::new();
    let mut start = 0;
    for i in 1..=focus.len() {
        let ends = i == focus.len() || focus[i].0.at_ms() - focus[i - 1].0.until_ms() > CLUSTER_GAP_MS;
        if ends {
            clusters.push(&focus[start..i]);
            start = i;
        }
    }
    clusters.into_iter().filter(|c| !c.is_empty()).collect()
}

fn push_key(keys: &mut Vec<(f64, View)>, t_ms: f64, view: View) {
    let t_ms = t_ms.max(keys[keys.len() - 1].0);
    keys.push((t_ms, view));
}

fn cursor_moves(focus: &[(&Event, Rect)], start: (f64, f64)) -> Vec<Move> {
    let mut moves = Vec::new();
    let (mut position, mut free_from) = (start, 0.0);
    for (event, target) in focus {
        let to = target.center();
        let end_ms = event.at_ms() - CURSOR_ARRIVES_EARLY_MS;
        let start_ms = (end_ms - CURSOR_TRAVEL_MS).max(free_from);
        if start_ms < end_ms && position != to {
            moves.push(Move { start_ms, end_ms, from: position, to });
        }
        position = to;
        free_from = event.until_ms();
    }
    moves
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
                Event::Pause { at_ms, until_ms } => (*at_ms, *until_ms),
                _ => (e.at_ms() - ZOOM_IN_LEAD_MS, e.until_ms() + HOLD_MS + ZOOM_OUT_MS),
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
                if end_ms - start_ms > 300.0 {
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

    const FRAME: Size = Size { w: 1920.0, h: 1080.0 };
    const BUTTON: Rect = Rect { x: 900.0, y: 500.0, w: 100.0, h: 32.0 };

    fn click(at_ms: f64, target: Rect) -> Event {
        Event::Click { at_ms, target }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.5
    }

    #[test]
    fn zooms_onto_a_clicked_button() {
        let plan = Plan::new(&[click(2_000.0, BUTTON)], 6_000.0, FRAME);

        let view = plan.camera_at(2_000.0);

        assert!(close(view.zoom(FRAME), MAX_ZOOM), "zoom was {}", view.zoom(FRAME));
        let (cx, cy) = view.center();
        assert!(close(cx, 950.0) && close(cy, 516.0), "centered on ({cx}, {cy})");
    }

    #[test]
    fn keeps_the_view_inside_the_frame_for_a_target_in_a_corner() {
        let corner = Rect { x: 1880.0, y: 4.0, w: 32.0, h: 32.0 };
        let plan = Plan::new(&[click(2_000.0, corner)], 6_000.0, FRAME);

        let view = plan.camera_at(2_000.0);

        assert!(view.zoom(FRAME) > 1.5);
        assert!(view.x >= 0.0 && view.y >= 0.0);
        assert!(view.x + view.w <= FRAME.w + 0.01 && view.y + view.h <= FRAME.h + 0.01);
    }

    #[test]
    fn shows_the_whole_frame_before_and_well_after_the_action() {
        let plan = Plan::new(&[click(2_000.0, BUTTON)], 10_000.0, FRAME);

        assert_eq!(plan.camera_at(0.0).zoom(FRAME), 1.0);
        assert_eq!(plan.camera_at(9_000.0).zoom(FRAME), 1.0);
    }

    #[test]
    fn pans_between_close_actions_without_zooming_out() {
        let other = Rect { x: 300.0, y: 200.0, w: 100.0, h: 32.0 };
        let plan = Plan::new(&[click(2_000.0, BUTTON), click(3_200.0, other)], 8_000.0, FRAME);

        for t in [2_000.0, 2_600.0, 3_200.0] {
            assert!(plan.camera_at(t).zoom(FRAME) > 1.5, "zoomed out at {t}");
        }
        let (cx, _) = plan.camera_at(3_200.0).center();
        assert!(cx < 950.0, "still centered on the first button at {cx}");
    }

    #[test]
    fn moves_the_cursor_onto_the_target_before_clicking() {
        let plan = Plan::new(&[click(2_000.0, BUTTON)], 6_000.0, FRAME);

        let at_click = plan.cursor_at(2_000.0);
        assert!(close(at_click.x, 950.0) && close(at_click.y, 516.0));
        let start = plan.cursor_at(0.0);
        let travelling = plan.cursor_at(1_600.0);
        assert!(travelling != start && (travelling.x, travelling.y) != (at_click.x, at_click.y));
    }

    #[test]
    fn presses_the_cursor_and_ripples_on_a_click() {
        let plan = Plan::new(&[click(2_000.0, BUTTON)], 6_000.0, FRAME);

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
        let plan = Plan::new(&events, 6_000.0, FRAME);

        assert!(close(plan.time.output_duration_ms(), 6_000.0));
        assert!(close(plan.time.source_ms(3_000.0), 3_000.0));
    }

    #[test]
    fn fast_forwards_an_idle_stretch() {
        let plan = Plan::new(&[click(1_000.0, BUTTON), click(9_000.0, BUTTON)], 12_000.0, FRAME);

        let output = plan.time.output_duration_ms();
        assert!(output < 10_000.0, "output was {output}");
        let samples: Vec<f64> = (0..=40).map(|i| plan.time.source_ms(output * i as f64 / 40.0)).collect();
        assert!(samples.windows(2).all(|w| w[1] >= w[0]), "source time went backwards");
        assert!(close(plan.time.source_ms(0.0), 0.0));
        assert!(close(plan.time.source_ms(output), 12_000.0));
    }

    #[test]
    fn cuts_an_idle_stretch_too_long_to_fast_forward() {
        let plan = Plan::new(&[click(1_000.0, BUTTON), click(120_000.0, BUTTON)], 124_000.0, FRAME);

        assert!(plan.time.output_duration_ms() < 15_000.0, "output was {}", plan.time.output_duration_ms());
    }

    #[test]
    fn keeps_a_scripted_pause_at_real_speed() {
        let events = [click(1_000.0, BUTTON), Event::Pause { at_ms: 1_000.0, until_ms: 9_000.0 }];
        let plan = Plan::new(&events, 9_000.0, FRAME);

        assert!(close(plan.time.output_duration_ms(), 9_000.0));
    }

    #[test]
    fn plays_typing_faster() {
        let field = Rect { x: 600.0, y: 400.0, w: 500.0, h: 40.0 };
        let events = [Event::Type { at_ms: 1_000.0, until_ms: 6_000.0, target: field }];
        let plan = Plan::new(&events, 9_000.0, FRAME);

        assert!(plan.time.output_duration_ms() < 8_000.0, "output was {}", plan.time.output_duration_ms());
    }
}
