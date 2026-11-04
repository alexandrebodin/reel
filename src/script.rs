use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::style::{Fill, Style};

/// A demo to record, read from a JSON file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Script {
    pub url: String,
    #[serde(default = "default_size")]
    pub size: [u32; 2],
    #[serde(default = "default_fps")]
    pub fps: u32,
    /// Device pixels per CSS pixel in the capture: text stays sharp through padding and zoom.
    #[serde(default = "default_scale")]
    pub scale: u32,
    #[serde(default)]
    pub style: Style,
    /// agent-browser state file whose live cookies sign the browser in. Only its cookies are used.
    #[serde(default)]
    pub state: Option<PathBuf>,
    /// Steps run before recording starts: navigation, sign-in, dismissing banners.
    #[serde(default)]
    pub setup: Vec<Step>,
    pub steps: Vec<Step>,
}

/// `target` is a CSS selector, or `text=Label` for the visible text of a button, link or menu item
/// (the topmost one when several match).
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    Click { target: String },
    Type { target: String, text: String },
    Pause { ms: u64 },
    WaitFor { target: String },
    Goto { url: String },
}

fn default_size() -> [u32; 2] {
    [1920, 1080]
}

fn default_fps() -> u32 {
    60
}

fn default_scale() -> u32 {
    2
}

impl Script {
    pub fn load(path: &Path) -> Result<Script> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut script: Script = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let base = path.parent().unwrap_or(Path::new("."));
        script.url = resolve_url(&script.url, base)?;
        for step in &mut script.steps {
            if let Step::Goto { url } = step {
                *url = resolve_url(url, base)?;
            }
        }
        script.state = script.state.map(|state| base.join(state));
        if let Fill::Image(image) = &mut script.style.background.fill {
            let path = base.join(&*image);
            *image = path.canonicalize().with_context(|| format!("background image {} not found", path.display()))?;
        }
        script.style = script.style.validated().with_context(|| format!("reading the style of {}", path.display()))?;
        Ok(script)
    }
}

/// A bare path is a page next to the script.
fn resolve_url(url: &str, base: &Path) -> Result<String> {
    if url.contains("://") || url.starts_with("data:") || url.starts_with("about:") {
        return Ok(url.to_string());
    }
    let path = base.join(url);
    let path = path.canonicalize().with_context(|| format!("page {} not found", path.display()))?;
    Ok(format!("file://{}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_steps_and_resolves_a_page_next_to_the_script() {
        let dir = std::env::temp_dir().join(format!("reel-script-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("page.html"), "<button>Go</button>").unwrap();
        std::fs::write(
            dir.join("demo.json"),
            r#"{ "url": "page.html", "steps": [
                { "do": "click", "target": "text=Go" },
                { "do": "type", "target": "input", "text": "hi" },
                { "do": "pause", "ms": 500 }
            ] }"#,
        )
        .unwrap();

        let script = Script::load(&dir.join("demo.json")).unwrap();

        assert!(script.url.starts_with("file:///") && script.url.ends_with("/page.html"));
        assert_eq!(script.size, [1920, 1080]);
        assert_eq!(script.fps, 60);
        assert_eq!(script.scale, 2);
        assert_eq!(
            script.steps,
            vec![
                Step::Click { target: "text=Go".into() },
                Step::Type { target: "input".into(), text: "hi".into() },
                Step::Pause { ms: 500 },
            ]
        );
    }

    #[test]
    fn reads_the_style_block() {
        let script: Script = serde_json::from_str(r#"{ "url": "about:blank", "style": { "zoom": { "level": 1.2 } }, "steps": [] }"#).unwrap();

        assert_eq!(script.style.zoom.level, 1.2);
    }

    #[test]
    fn resolves_a_background_image_next_to_the_script_and_rejects_a_missing_one() {
        let dir = std::env::temp_dir().join(format!("reel-script-image-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("wall.png"), "").unwrap();
        let demo = |image: &str| format!(r#"{{ "url": "about:blank", "style": {{ "background": {{ "fill": "{image}" }} }}, "steps": [] }}"#);
        std::fs::write(dir.join("found.json"), demo("wall.png")).unwrap();
        std::fs::write(dir.join("missing.json"), demo("gone.png")).unwrap();

        let script = Script::load(&dir.join("found.json")).unwrap();

        assert_eq!(script.style.background.fill, Fill::Image(dir.canonicalize().unwrap().join("wall.png")));
        assert!(format!("{:#}", Script::load(&dir.join("missing.json")).unwrap_err()).contains("gone.png"));
    }

    #[test]
    fn rejects_an_unknown_step() {
        let parsed: Result<Script, _> = serde_json::from_str(r#"{ "url": "about:blank", "steps": [{ "do": "dance" }] }"#);
        assert!(parsed.is_err());
    }
}
