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

/// What the page's cursor would look like over an element.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pointer {
    #[default]
    Arrow,
    Hand,
    Text,
}

impl Pointer {
    /// Reads a computed CSS `cursor` value.
    pub fn from_css(cursor: Option<&str>) -> Pointer {
        match cursor {
            Some("pointer") => Pointer::Hand,
            Some("text") | Some("vertical-text") => Pointer::Text,
            _ => Pointer::Arrow,
        }
    }
}

/// What happened on screen and when, in milliseconds from the start of the raw video.
/// `target` is what the cursor goes to; `frame` is what the camera should keep in view.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    Click {
        at_ms: f64,
        target: Rect,
        frame: Rect,
        #[serde(default)]
        pointer: Pointer,
    },
    Type {
        at_ms: f64,
        until_ms: f64,
        target: Rect,
        frame: Rect,
        #[serde(default)]
        pointer: Pointer,
    },
    /// Something appeared that the viewer should look at: a dialog, a menu, a result.
    Focus { at_ms: f64, frame: Rect },
    /// A pause the script asked for so viewers can read: never sped up.
    Pause { at_ms: f64, until_ms: f64 },
}

impl Event {
    pub fn at_ms(&self) -> f64 {
        match self {
            Event::Click { at_ms, .. } | Event::Type { at_ms, .. } | Event::Focus { at_ms, .. } | Event::Pause { at_ms, .. } => *at_ms,
        }
    }

    pub fn until_ms(&self) -> f64 {
        match self {
            Event::Click { at_ms, .. } | Event::Focus { at_ms, .. } => *at_ms,
            Event::Type { until_ms, .. } | Event::Pause { until_ms, .. } => *until_ms,
        }
    }

    pub fn cursor_target(&self) -> Option<Rect> {
        match self {
            Event::Click { target, .. } | Event::Type { target, .. } => Some(*target),
            Event::Focus { .. } | Event::Pause { .. } => None,
        }
    }

    pub fn pointer(&self) -> Pointer {
        match self {
            Event::Click { pointer, .. } | Event::Type { pointer, .. } => *pointer,
            Event::Focus { .. } | Event::Pause { .. } => Pointer::Arrow,
        }
    }

    pub fn camera_frame(&self) -> Option<Rect> {
        match self {
            Event::Click { frame, .. } | Event::Type { frame, .. } | Event::Focus { frame, .. } => Some(*frame),
            Event::Pause { .. } => None,
        }
    }

    pub fn shifted(&self, by_ms: f64) -> Event {
        match self.clone() {
            Event::Click { at_ms, target, frame, pointer } => Event::Click { at_ms: at_ms + by_ms, target, frame, pointer },
            Event::Type { at_ms, until_ms, target, frame, pointer } => Event::Type { at_ms: at_ms + by_ms, until_ms: until_ms + by_ms, target, frame, pointer },
            Event::Focus { at_ms, frame } => Event::Focus { at_ms: at_ms + by_ms, frame },
            Event::Pause { at_ms, until_ms } => Event::Pause { at_ms: at_ms + by_ms, until_ms: until_ms + by_ms },
        }
    }
}
