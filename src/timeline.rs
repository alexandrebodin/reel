use serde::{Deserialize, Serialize};

use crate::events::{Event, Pointer, Rect};
use crate::plan::{overview, Size, View};
use crate::style::Style;

/// What happens to the camera, the clock and the cursor over a take: everything an editor can
/// change. The automatic pass fills it from the event log; a render only ever reads it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Timeline {
    pub zooms: Vec<ZoomSpan>,
    pub speeds: Vec<SpeedSpan>,
    pub moves: Vec<Move>,
    pub clicks: Vec<Click>,
    pub typing: Vec<Typing>,
}

/// The camera on one spot. Zooms that touch pan from one to the next without zooming out.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZoomSpan {
    pub start_ms: f64,
    pub end_ms: f64,
    /// Where the camera looks, in CSS pixels.
    pub x: f64,
    pub y: f64,
    pub level: f64,
}

impl ZoomSpan {
    pub fn view(&self, frame: Size) -> View {
        let (w, h) = (frame.w / self.level, frame.h / self.level);
        View { x: (self.x - w / 2.0).clamp(0.0, frame.w - w), y: (self.y - h / 2.0).clamp(0.0, frame.h - h), w, h }
    }
}

/// A stretch of the take played at another speed. Everything outside these plays at real speed.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeedSpan {
    pub start_ms: f64,
    pub end_ms: f64,
    /// `None` cuts the stretch out.
    pub speed: Option<f64>,
}

/// The cursor travelling to a spot. It leaves from wherever the previous move ended.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Move {
    pub start_ms: f64,
    pub end_ms: f64,
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub pointer: Pointer,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Click {
    pub at_ms: f64,
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Typing {
    pub start_ms: f64,
    pub end_ms: f64,
}

pub const ZOOM_IN_MS: f64 = 700.0;
pub const UNZOOM_MS: f64 = 600.0;
pub const CURSOR_ARRIVES_EARLY_MS: f64 = 120.0;
const FOCUS_FOLLOWS_MS: f64 = 800.0;
const FIT_MARGIN: f64 = 1.25;
const VIEW_MARGIN: f64 = 24.0;
const HOLD_MS: f64 = 1_200.0;
const MIN_ZOOM_STAY_MS: f64 = 700.0;
const CLUSTER_GAP_MS: f64 = 2_500.0;
const LAST_FRAME_MS: f64 = 200.0;
const CURSOR_TRAVEL_MS: f64 = 650.0;
const MIN_CURSOR_TRAVEL_MS: f64 = 80.0;
const IDLE_EDGE_MS: f64 = 400.0;
const IDLE_TARGET_MS: f64 = 600.0;
const MIN_IDLE_SPEED: f64 = 3.0;
const MAX_IDLE_SPEED: f64 = 12.0;
const TYPING_SPEED: f64 = 2.0;
const MIN_FAST_TYPING_MS: f64 = 1_500.0;

/// Where the cursor rests before its first move.
pub fn cursor_start(frame: Size) -> (f64, f64) {
    (frame.w * 0.62, frame.h * 0.72)
}

impl Timeline {
    /// The automatic pass over a take's event log.
    pub fn auto(events: &[Event], duration_ms: f64, frame: Size, style: &Style) -> Timeline {
        let (zooms, moves) = choreograph(events, duration_ms, cursor_start(frame), frame, style);
        let clicks = events.iter().filter_map(|e| match e {
            Event::Click { at_ms, target, .. } => {
                let (x, y) = target.center();
                Some(Click { at_ms: *at_ms, x, y })
            }
            _ => None,
        });
        let typing = events.iter().filter_map(|e| match e {
            Event::Type { at_ms, until_ms, .. } => Some(Typing { start_ms: *at_ms, end_ms: *until_ms }),
            _ => None,
        });
        Timeline { zooms, speeds: auto_speeds(events, duration_ms), moves, clicks: clicks.collect(), typing: typing.collect() }
    }
}

pub fn focus_view(target: Rect, frame: Size, max_zoom: f64) -> View {
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
    zooms: Vec<ZoomSpan>,
    wide: View,
    shown: View,
    free_from: f64,
    frame: Size,
}

impl Camera {
    /// Moves to `view` from `start_ms` on, and returns when it gets there.
    fn move_to(&mut self, start_ms: f64, length_ms: f64, view: View) -> f64 {
        let start_ms = start_ms.max(self.free_from);
        let arrives = start_ms + length_ms;
        if self.shown != self.wide {
            let last = self.zooms.len() - 1;
            self.zooms[last].end_ms = if view == self.wide { arrives } else { start_ms };
        }
        if view != self.wide {
            let (x, y) = view.center();
            self.zooms.push(ZoomSpan { start_ms, end_ms: arrives, x, y, level: view.zoom(self.frame) });
        }
        self.shown = view;
        self.free_from = arrives;
        arrives
    }
}

/// Decides the camera and the cursor together, because one rule ties them: the cursor only travels
/// through what the viewer can see, so a target out of view means zooming out first. The camera
/// comes back in once the click has landed, and stays still for anything already in view.
fn choreograph(events: &[Event], duration_ms: f64, cursor_start: (f64, f64), frame: Size, style: &Style) -> (Vec<ZoomSpan>, Vec<Move>) {
    let pauses = pauses(events);
    let mut actions: Vec<&Event> = events.iter().filter(|e| e.camera_frame().is_some()).collect();
    actions.sort_by(|a, b| a.at_ms().total_cmp(&b.at_ms()));

    let wide = overview(frame);
    let mut camera = Camera { zooms: Vec::new(), wide, shown: wide, free_from: 0.0, frame };
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
                moves.push(Move { start_ms: travel_ms.min(arrive_ms - MIN_CURSOR_TRAVEL_MS), end_ms: arrive_ms, x: to.0, y: to.1, pointer: event.pointer() });
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
    (camera.zooms, moves)
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

/// Idle stretches play fast or are cut, typing plays faster.
fn auto_speeds(events: &[Event], duration_ms: f64) -> Vec<SpeedSpan> {
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
                zones.push(SpeedSpan { start_ms, end_ms, speed: Some(TYPING_SPEED) });
            }
        }
    }
    zones.sort_by(|a, b| a.start_ms.total_cmp(&b.start_ms));
    zones
}

/// Keeps a short real-speed edge on both sides so the jump never looks abrupt.
pub fn idle_zone(start_ms: f64, end_ms: f64) -> Option<SpeedSpan> {
    let (start_ms, end_ms) = (start_ms + IDLE_EDGE_MS, end_ms - IDLE_EDGE_MS);
    let length = end_ms - start_ms;
    if length <= 200.0 {
        return None;
    }
    let speed = length / IDLE_TARGET_MS;
    let speed = if speed > MAX_IDLE_SPEED { None } else { Some(speed.max(MIN_IDLE_SPEED)) };
    Some(SpeedSpan { start_ms, end_ms, speed })
}
