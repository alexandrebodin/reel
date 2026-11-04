use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// How the video looks: the `style` block of a demo, overridable from the CLI.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Style {
    pub zoom: Zoom,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Zoom {
    /// The furthest the camera goes in: 1 never zooms.
    pub level: f64,
}

impl Default for Zoom {
    fn default() -> Zoom {
        Zoom { level: 1.4 }
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
