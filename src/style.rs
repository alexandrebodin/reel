use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// How the video looks: the `style` block of a demo, overridable from the CLI.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Style {
    pub zoom: Zoom,
    pub background: Background,
    pub cursor: CursorStyle,
    pub clicks: Clicks,
    pub motion_blur: MotionBlur,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MotionBlur {
    Off,
    Low,
    Medium,
    High,
}

impl Default for MotionBlur {
    fn default() -> MotionBlur {
        MotionBlur::Off
    }
}

impl MotionBlur {
    /// Samples averaged per frame, and the share of the frame's time they span.
    pub fn shutter(self) -> (u32, f64) {
        match self {
            MotionBlur::Off => (1, 0.0),
            MotionBlur::Low => (4, 0.35),
            MotionBlur::Medium => (8, 0.6),
            MotionBlur::High => (12, 1.0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZoomAnimation {
    /// Eases in and out over the whole move.
    Smooth,
    /// Darts most of the way, then settles with a slight overshoot.
    Dynamic,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RippleStyle {
    Filled,
    Outline,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Clicks {
    /// The cursor shrinks a little while the button is down.
    pub press: bool,
    pub ripple: bool,
    pub color: Rgb,
    pub style: RippleStyle,
}

impl Default for Clicks {
    fn default() -> Clicks {
        Clicks { press: true, ripple: true, color: Rgb([255, 106, 61]), style: RippleStyle::Filled }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorLook {
    WhiteArrow,
    BlackArrow,
    Dot,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorMotion {
    /// A slight arc with a quick start and stop, like a hand.
    Natural,
    /// A straight line with a gentle start and stop.
    Smooth,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CursorStyle {
    pub show: bool,
    /// Multiplies the default size.
    pub size: f64,
    pub style: CursorLook,
    pub motion: CursorMotion,
    pub hide_when_idle: bool,
    pub hide_when_typing: bool,
    /// Turns into a hand or a text cursor over elements that ask for one.
    pub adaptive: bool,
}

impl Default for CursorStyle {
    fn default() -> CursorStyle {
        CursorStyle { show: true, size: 1.0, style: CursorLook::WhiteArrow, motion: CursorMotion::Natural, hide_when_idle: false, hide_when_typing: false, adaptive: true }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Rgb(pub [u8; 3]);

impl TryFrom<String> for Rgb {
    type Error = anyhow::Error;

    fn try_from(text: String) -> Result<Rgb> {
        let named = match text.as_str() {
            "white" => Some([255, 255, 255]),
            "black" => Some([0, 0, 0]),
            "blue" => Some([10, 132, 255]),
            "red" => Some([255, 69, 58]),
            "orange" => Some([255, 106, 61]),
            "yellow" => Some([255, 214, 10]),
            "green" => Some([48, 209, 88]),
            "purple" => Some([191, 90, 242]),
            "pink" => Some([255, 55, 95]),
            _ => None,
        };
        if let Some(rgb) = named {
            return Ok(Rgb(rgb));
        }
        let hex = text.strip_prefix('#').filter(|h| h.len() == 6 && h.is_ascii()).ok_or_else(|| anyhow!("{text} is not a colour: use #rrggbb or a name"))?;
        let channel = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).map_err(|_| anyhow!("{text} is not a colour"));
        Ok(Rgb([channel(0)?, channel(2)?, channel(4)?]))
    }
}

impl From<Rgb> for String {
    fn from(Rgb([r, g, b]): Rgb) -> String {
        format!("#{r:02x}{g:02x}{b:02x}")
    }
}

/// What sits behind the video, written as one string: `none`, `#rrggbb`, `#from,#to` for a diagonal
/// gradient, or the path of an image.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Fill {
    None,
    Color(Rgb),
    Gradient(Rgb, Rgb),
    Image(PathBuf),
}

impl TryFrom<String> for Fill {
    type Error = anyhow::Error;

    fn try_from(text: String) -> Result<Fill> {
        if text == "none" {
            return Ok(Fill::None);
        }
        if let Some((from, to)) = text.split_once(',') {
            return Ok(Fill::Gradient(from.trim().to_string().try_into()?, to.trim().to_string().try_into()?));
        }
        if text.starts_with('#') {
            return Ok(Fill::Color(text.try_into()?));
        }
        Ok(Fill::Image(PathBuf::from(text)))
    }
}

impl From<Fill> for String {
    fn from(fill: Fill) -> String {
        match fill {
            Fill::None => "none".to_string(),
            Fill::Color(color) => color.into(),
            Fill::Gradient(from, to) => format!("{},{}", String::from(from), String::from(to)),
            Fill::Image(path) => path.display().to_string(),
        }
    }
}

/// `auto` keeps the video's own shape; `w:h` puts it in a frame of that shape.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Aspect {
    Auto,
    Ratio(u32, u32),
}

impl TryFrom<String> for Aspect {
    type Error = anyhow::Error;

    fn try_from(text: String) -> Result<Aspect> {
        if text == "auto" {
            return Ok(Aspect::Auto);
        }
        let ratio = text.split_once(':').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)));
        match ratio {
            Some((w, h)) if w > 0 && h > 0 => Ok(Aspect::Ratio(w, h)),
            _ => bail!("{text} is not an aspect ratio: use auto or w:h"),
        }
    }
}

impl From<Aspect> for String {
    fn from(aspect: Aspect) -> String {
        match aspect {
            Aspect::Auto => "auto".to_string(),
            Aspect::Ratio(w, h) => format!("{w}:{h}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Background {
    pub fill: Fill,
    pub aspect: Aspect,
    /// Percent of the frame left around the video on each side.
    pub padding: f64,
    /// Strength of the drop shadow, 0 to 100.
    pub shadow: f64,
    /// Corner radius as a percent of the video's short side.
    pub corners: f64,
}

impl Default for Background {
    fn default() -> Background {
        Background {
            fill: Fill::Gradient(Rgb([45, 27, 105]), Rgb([226, 88, 31])),
            aspect: Aspect::Auto,
            padding: 5.0,
            shadow: 50.0,
            corners: 1.852,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Zoom {
    /// The furthest the camera goes in: 1 never zooms.
    pub level: f64,
    pub animation: ZoomAnimation,
}

impl Default for Zoom {
    fn default() -> Zoom {
        Zoom { level: 1.4, animation: ZoomAnimation::Smooth }
    }
}

impl Style {
    /// Applies `dotted.key=value` overrides. A value that is not JSON is a string.
    pub fn overridden(&self, overrides: &[String]) -> Result<Style> {
        let mut tree = serde_json::to_value(self)?;
        for assignment in overrides {
            let Some((key, value)) = assignment.split_once('=') else {
                bail!("--style {assignment}: expected key=value");
            };
            let value = serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_string()));
            set(&mut tree, key, value);
        }
        let style: Style = serde_json::from_value(tree).context("reading --style")?;
        style.validated()
    }

    pub fn validated(self) -> Result<Style> {
        if !(1.0..=8.0).contains(&self.zoom.level) {
            bail!("zoom.level must be between 1 and 8, not {}", self.zoom.level);
        }
        if !(0.25..=8.0).contains(&self.cursor.size) {
            bail!("cursor.size must be between 0.25 and 8, not {}", self.cursor.size);
        }
        let background = &self.background;
        if !(0.0..45.0).contains(&background.padding) {
            bail!("background.padding must be a percent below 45, not {}", background.padding);
        }
        if !(0.0..=100.0).contains(&background.shadow) {
            bail!("background.shadow must be between 0 and 100, not {}", background.shadow);
        }
        if !(0.0..=50.0).contains(&background.corners) {
            bail!("background.corners must be between 0 and 50, not {}", background.corners);
        }
        Ok(self)
    }
}

fn set(tree: &mut Value, key: &str, value: Value) {
    let (head, rest) = key.split_once('.').map_or((key, None), |(head, rest)| (head, Some(rest)));
    if !tree.is_object() {
        *tree = Value::Object(Map::new());
    }
    let slot = tree.as_object_mut().expect("just made an object").entry(head).or_insert(Value::Null);
    match rest {
        Some(rest) => set(slot, rest, value),
        None => *slot = value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(overrides: &[&str]) -> Result<Style> {
        let overrides: Vec<String> = overrides.iter().map(|o| o.to_string()).collect();
        Style::default().overridden(&overrides)
    }

    #[test]
    fn a_missing_block_is_todays_look() {
        let style: Style = serde_json::from_str("{}").unwrap();

        assert_eq!(style, Style::default());
        assert_eq!(style.zoom.level, 1.4);
    }

    #[test]
    fn overrides_a_nested_option_and_keeps_the_rest() {
        let style = with(&["zoom.level=1.2"]).unwrap();

        assert_eq!(style.zoom.level, 1.2);
    }

    #[test]
    fn reads_every_kind_of_background() {
        let fill = |text: &str| with(&[&format!("background.fill={text}")]).unwrap().background.fill;

        assert_eq!(fill("none"), Fill::None);
        assert_eq!(fill("#102030"), Fill::Color(Rgb([16, 32, 48])));
        assert_eq!(fill("#000000,#ffffff"), Fill::Gradient(Rgb([0, 0, 0]), Rgb([255, 255, 255])));
        assert_eq!(fill("walls/sea.jpg"), Fill::Image(PathBuf::from("walls/sea.jpg")));
    }

    #[test]
    fn keeps_every_background_through_an_override_of_something_else() {
        for fill in ["none", "#102030", "#000000,#ffffff", "walls/sea.jpg"] {
            let style = with(&[&format!("background.fill={fill}"), "zoom.level=1.2"]).unwrap();
            let again = style.overridden(&["zoom.level=1.1".to_string()]).unwrap();

            assert_eq!(again.background.fill, style.background.fill);
        }
    }

    #[test]
    fn reads_an_aspect_ratio() {
        assert_eq!(with(&["background.aspect=16:9"]).unwrap().background.aspect, Aspect::Ratio(16, 9));
        assert_eq!(with(&["background.aspect=auto"]).unwrap().background.aspect, Aspect::Auto);
        assert!(with(&["background.aspect=wide"]).is_err());
        assert!(with(&["background.aspect=0:9"]).is_err());
    }

    #[test]
    fn reads_colours_by_name_and_hex_and_rejects_the_rest() {
        assert_eq!(Rgb::try_from("blue".to_string()).unwrap(), Rgb([10, 132, 255]));
        assert_eq!(Rgb::try_from("#FF0080".to_string()).unwrap(), Rgb([255, 0, 128]));
        assert!(Rgb::try_from("#ff00".to_string()).is_err());
        assert!(Rgb::try_from("#gg0000".to_string()).is_err());
        assert!(Rgb::try_from("mauve".to_string()).is_err());
    }

    #[test]
    fn rejects_background_values_out_of_range() {
        assert!(with(&["background.padding=45"]).is_err());
        assert!(with(&["background.padding=-1"]).is_err());
        assert!(with(&["background.padding=44"]).is_ok());
        assert!(with(&["background.shadow=101"]).is_err());
        assert!(with(&["background.corners=51"]).is_err());
    }

    #[test]
    fn reads_cursor_options() {
        let style = with(&["cursor.size=2", "cursor.style=black_arrow", "cursor.motion=smooth", "cursor.hide_when_typing=true"]).unwrap();

        assert_eq!(style.cursor.size, 2.0);
        assert_eq!(style.cursor.style, CursorLook::BlackArrow);
        assert_eq!(style.cursor.motion, CursorMotion::Smooth);
        assert!(style.cursor.hide_when_typing);
        assert!(with(&["cursor.size=0"]).is_err());
        assert!(with(&["cursor.style=hand"]).is_err());
    }

    #[test]
    fn reads_click_options() {
        let style = with(&["clicks.press=false", "clicks.color=blue", "clicks.style=outline"]).unwrap();

        assert!(!style.clicks.press);
        assert_eq!(style.clicks.color, Rgb([10, 132, 255]));
        assert_eq!(style.clicks.style, RippleStyle::Outline);
        assert!(with(&["clicks.color=mauve"]).is_err());
    }

    #[test]
    fn reads_motion_options() {
        let style = with(&["motion_blur=medium", "zoom.animation=dynamic"]).unwrap();

        assert_eq!(style.motion_blur, MotionBlur::Medium);
        assert_eq!(style.zoom.animation, ZoomAnimation::Dynamic);
        assert_eq!(Style::default().motion_blur, MotionBlur::Off);
        assert!(with(&["motion_blur=extreme"]).is_err());
    }

    #[test]
    fn rejects_a_mistyped_option() {
        let error = with(&["zoom.levle=1.2"]).unwrap_err();

        assert!(format!("{error:#}").contains("levle"), "{error:#}");
    }

    #[test]
    fn rejects_an_override_without_a_value() {
        assert!(with(&["zoom.level"]).is_err());
    }

    #[test]
    fn rejects_a_zoom_below_the_whole_frame() {
        assert!(with(&["zoom.level=0.5"]).is_err());
    }
}
