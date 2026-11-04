pub mod capture;
pub mod events;
pub mod plan;
pub mod render;
pub mod screencast;
pub mod script;
pub mod server;
pub mod style;
pub mod timeline;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use capture::{align_to_video, capture, AgentBrowser, Browser, Pacing, Recording};
use render::{plan_for, render, video_duration_ms};
use script::Script;

/// Records the demo a script describes and renders it. Keeps the raw take and its event log
/// next to the script, in `out/<name>/`, so `render_file` can re-render without recording again.
pub fn record(script_path: &Path, output: Option<PathBuf>, style_overrides: &[String]) -> Result<PathBuf> {
    let mut script = Script::load(script_path)?;
    script.style = script.style.overridden(style_overrides)?;
    let name = script_path.file_stem().and_then(|s| s.to_str()).unwrap_or("demo").to_string();
    let base = script_path.parent().unwrap_or(Path::new("."));
    let dir = base.join("out").join(&name);
    fs::create_dir_all(&dir)?;
    let dir = dir.canonicalize()?;
    let video = dir.join("raw.mp4");

    let mut browser = AgentBrowser::new(&format!("reel-{name}"), script.scale);
    let captured = capture(&script, &mut browser, &video, &Pacing::default());
    let _ = browser.run(&["close"], None);
    let mut recording = captured?;
    align_to_video(&mut recording, video_duration_ms(&video)?);
    let recording_path = dir.join("recording.json");
    fs::write(&recording_path, serde_json::to_string_pretty(&recording)?)?;

    let output = output.unwrap_or_else(|| dir.join(format!("{name}.mp4")));
    render(&recording, &plan_for(&recording), &output)?;
    Ok(output)
}

pub fn render_file(recording_path: &Path, output: Option<PathBuf>, style_overrides: &[String]) -> Result<PathBuf> {
    let mut recording = Recording::load(recording_path)?;
    let style = recording.style.overridden(style_overrides)?;
    if recording.timeline.is_some() && style.zoom.level != recording.style.zoom.level {
        bail!("zoom.level only shapes the automatic timeline, and {} has been edited: change the zooms in the editor", recording_path.display());
    }
    recording.style = style;
    let output = output.unwrap_or_else(|| recording_path.with_file_name("render.mp4"));
    render(&recording, &plan_for(&recording), &output)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(name: &str, recording: &Recording) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("reel-lib-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("recording.json");
        fs::write(&path, serde_json::to_string(recording).unwrap()).unwrap();
        path
    }

    fn recording(timeline: Option<timeline::Timeline>) -> Recording {
        Recording { video: "/nonexistent/raw.mp4".into(), size: [320, 180], scale: 1, fps: 10, duration_ms: 1_000.0, events: vec![], style: Default::default(), timeline, imported: false }
    }

    #[test]
    fn refuses_a_zoom_level_override_on_an_edited_take_and_says_why() {
        let path = written("edited", &recording(Some(Default::default())));

        let error = render_file(&path, None, &["zoom.level=2".to_string()]).unwrap_err();

        assert!(error.to_string().contains("zoom.level"), "{error}");
    }

    #[test]
    fn takes_a_zoom_level_override_on_an_unedited_take() {
        let path = written("unedited", &recording(None));

        let error = render_file(&path, None, &["zoom.level=2".to_string()]).unwrap_err();

        assert!(!error.to_string().contains("zoom.level"), "{error}");
    }

    #[test]
    fn refuses_a_recording_whose_saved_timeline_is_invalid() {
        let zoom = timeline::ZoomSpan { start_ms: 5.0, end_ms: 1.0, x: 0.0, y: 0.0, level: 2.0 };
        let path = written("invalid", &recording(Some(timeline::Timeline { zooms: vec![zoom], ..Default::default() })));

        assert!(Recording::load(&path).is_err());
    }
}
