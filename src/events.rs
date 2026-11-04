use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// What happened on screen and when, in milliseconds from the start of the raw video.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    Click { at_ms: f64, target: Rect },
    Type { at_ms: f64, until_ms: f64, target: Rect },
    /// A pause the script asked for so viewers can read: never sped up.
    Pause { at_ms: f64, until_ms: f64 },
}

impl Event {
    pub fn at_ms(&self) -> f64 {
        match self {
            Event::Click { at_ms, .. } | Event::Type { at_ms, .. } | Event::Pause { at_ms, .. } => *at_ms,
        }
    }

    pub fn until_ms(&self) -> f64 {
        match self {
            Event::Click { at_ms, .. } => *at_ms,
            Event::Type { until_ms, .. } | Event::Pause { until_ms, .. } => *until_ms,
        }
    }

    pub fn target(&self) -> Option<Rect> {
        match self {
            Event::Click { target, .. } | Event::Type { target, .. } => Some(*target),
            Event::Pause { .. } => None,
        }
    }

    pub fn shifted(&self, by_ms: f64) -> Event {
        match self.clone() {
            Event::Click { at_ms, target } => Event::Click { at_ms: at_ms + by_ms, target },
            Event::Type { at_ms, until_ms, target } => Event::Type { at_ms: at_ms + by_ms, until_ms: until_ms + by_ms, target },
            Event::Pause { at_ms, until_ms } => Event::Pause { at_ms: at_ms + by_ms, until_ms: until_ms + by_ms },
        }
    }
}
