pub mod capture;
pub mod events;
pub mod plan;
pub mod render;
pub mod script;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use capture::{align_to_video, capture, AgentBrowser, Browser, Pacing, Recording};
use render::{plan_for, render, video_duration_ms, Style};
use script::Script;

/// Records the demo a script describes and renders it. Keeps the raw take and its event log
/// next to the script, in `out/<name>/`, so `render_file` can re-render without recording again.
pub fn record(script_path: &Path, output: Option<PathBuf>) -> Result<PathBuf> {
    let script = Script::load(script_path)?;
    let name = script_path.file_stem().and_then(|s| s.to_str()).unwrap_or("demo").to_string();
    let base = script_path.parent().unwrap_or(Path::new("."));
    let dir = base.join("out").join(&name);
    fs::create_dir_all(&dir)?;
    let dir = dir.canonicalize()?;
    let video = dir.join("raw.webm");

    let mut browser = AgentBrowser::new(&format!("reel-{name}"));
    let captured = capture(&script, &mut browser, &video, &Pacing::default());
    let _ = browser.run(&["close"], None);
    let mut recording = captured?;
    align_to_video(&mut recording, video_duration_ms(&video)?);
    let recording_path = dir.join("recording.json");
    fs::write(&recording_path, serde_json::to_string_pretty(&recording)?)?;

    let output = output.unwrap_or_else(|| dir.join(format!("{name}.mp4")));
    render(&recording, &plan_for(&recording), &output, &Style::default())?;
    Ok(output)
}

pub fn render_file(recording_path: &Path, output: Option<PathBuf>) -> Result<PathBuf> {
    let text = fs::read_to_string(recording_path).with_context(|| format!("reading {}", recording_path.display()))?;
    let recording: Recording = serde_json::from_str(&text)?;
    let output = output.unwrap_or_else(|| recording_path.with_file_name("render.mp4"));
    render(&recording, &plan_for(&recording), &output, &Style::default())?;
    Ok(output)
}
