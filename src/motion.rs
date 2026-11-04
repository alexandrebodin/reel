use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::capture::Recording;
use crate::events::Rect;
use crate::plan::{overview, Size};
use crate::style::Style;
use crate::timeline::{focus_view, idle_zone, Timeline, ZoomSpan};

const SAMPLE_WIDTH: u32 = 160;
const SAMPLES_PER_SECOND: f64 = 5.0;
/// Below this, a pixel's change is compression noise rather than something happening.
const CHANGED_LEVEL: u8 = 12;
const SAME_BURST_GAP_MS: f64 = 1_000.0;
const MIN_IDLE_MS: f64 = 1_500.0;
const MIN_ZOOM_BURST_MS: f64 = 400.0;
const MAX_ZOOM_AREA: f64 = 0.4;
const ZOOM_HOLD_MS: f64 = 1_800.0;
const MIN_ZOOM_MS: f64 = 1_300.0;
const RETINA_WIDTH: u32 = 2560;

/// A stretch of the video where the picture keeps changing, and the part of the frame it changes in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Burst {
    pub start_ms: f64,
    pub end_ms: f64,
    /// As shares of the frame, from 0 to 1.
    pub region: Rect,
}

/// Finds the bursts in evenly spaced grey frames of `width` pixels across.
pub fn bursts(frames: &[Vec<u8>], width: usize, step_ms: f64) -> Vec<Burst> {
    let mut found: Vec<Burst> = Vec::new();
    for (i, pair) in frames.windows(2).enumerate() {
        let Some(region) = changed_region(&pair[0], &pair[1], width) else { continue };
        let (start_ms, end_ms) = (i as f64 * step_ms, (i + 1) as f64 * step_ms);
        match found.last_mut() {
            Some(burst) if start_ms - burst.end_ms <= SAME_BURST_GAP_MS => {
                burst.end_ms = end_ms;
                burst.region = merged(burst.region, region);
            }
            _ => found.push(Burst { start_ms, end_ms, region }),
        }
    }
    found
}

fn changed_region(a: &[u8], b: &[u8], width: usize) -> Option<Rect> {
    let height = a.len() / width;
    let (mut left, mut top, mut right, mut bottom) = (width, height, 0, 0);
    for (at, (x, y)) in a.iter().zip(b).enumerate() {
        if x.abs_diff(*y) >= CHANGED_LEVEL {
            let (column, row) = (at % width, at / width);
            (left, top, right, bottom) = (left.min(column), top.min(row), right.max(column + 1), bottom.max(row + 1));
        }
    }
    (right > left).then(|| {
        let (w, h) = (width as f64, height as f64);
        Rect { x: left as f64 / w, y: top as f64 / h, w: (right - left) as f64 / w, h: (bottom - top) as f64 / h }
    })
}

/// A dialog's scrim changes the whole frame for a moment and the typing after it only the field:
/// the field is what to look at, so a change confined to part of the frame outranks a wide one.
fn merged(a: Rect, b: Rect) -> Rect {
    match (is_wide(a), is_wide(b)) {
        (true, false) => b,
        (false, true) => a,
        _ => union(a, b),
    }
}

fn is_wide(region: Rect) -> bool {
    region.w * region.h > MAX_ZOOM_AREA
}

fn union(a: Rect, b: Rect) -> Rect {
    let (left, top) = (a.x.min(b.x), a.y.min(b.y));
    let (right, bottom) = ((a.x + a.w).max(b.x + b.w), (a.y + a.h).max(b.y + b.h));
    Rect { x: left, y: top, w: right - left, h: bottom - top }
}

/// A first guess for someone to correct: stretches with nothing happening play fast, and a change
/// confined to part of the frame gets a zoom. Pixels cannot say where clicks are, so none are made.
pub fn timeline_from(bursts: &[Burst], duration_ms: f64, frame: Size, style: &Style) -> Timeline {
    let mut speeds = Vec::new();
    let mut free_from = 0.0;
    for (start, end) in bursts.iter().map(|b| (b.start_ms, b.end_ms)).chain(std::iter::once((duration_ms, duration_ms))) {
        if start - free_from >= MIN_IDLE_MS {
            speeds.extend(idle_zone(free_from, start));
        }
        free_from = end;
    }
    let mut zooms: Vec<ZoomSpan> = Vec::new();
    for burst in bursts {
        let Rect { x, y, w, h } = burst.region;
        if burst.end_ms - burst.start_ms < MIN_ZOOM_BURST_MS || w * h > MAX_ZOOM_AREA {
            continue;
        }
        let view = focus_view(Rect { x: x * frame.w, y: y * frame.h, w: w * frame.w, h: h * frame.h }, frame, style.zoom.level);
        if view == overview(frame) {
            continue;
        }
        let start_ms = zooms.last().map_or(burst.start_ms, |z: &ZoomSpan| z.end_ms.max(burst.start_ms));
        let end_ms = (burst.end_ms + ZOOM_HOLD_MS).max(start_ms + MIN_ZOOM_MS).min(duration_ms);
        if end_ms > start_ms {
            let (cx, cy) = view.center();
            zooms.push(ZoomSpan { start_ms, end_ms, x: cx, y: cy, level: view.zoom(frame) });
        }
    }
    Timeline { zooms, speeds, ..Timeline::default() }
}

/// Runs the first guess on a video, at the zoom level the style asks for.
pub fn analyze(recording: &Recording) -> Result<Timeline> {
    let [width, height] = recording.size;
    let sample_height = ((SAMPLE_WIDTH as f64 * height as f64 / width as f64 / 2.0).round() as u32 * 2).max(2);
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&recording.video)
        .args(["-vf", &format!("fps={SAMPLES_PER_SECOND},scale={SAMPLE_WIDTH}:{sample_height}"), "-f", "rawvideo", "-pix_fmt", "gray", "-"])
        .output()
        .context("starting ffmpeg")?;
    if !output.status.success() {
        bail!("could not read {}: {}", recording.video.display(), String::from_utf8_lossy(&output.stderr));
    }
    let frames: Vec<Vec<u8>> = output.stdout.chunks_exact((SAMPLE_WIDTH * sample_height) as usize).map(<[u8]>::to_vec).collect();
    let found = bursts(&frames, SAMPLE_WIDTH as usize, 1_000.0 / SAMPLES_PER_SECOND);
    Ok(timeline_from(&found, recording.duration_ms, recording.frame(), &recording.style))
}

/// Makes a project for an existing video, next to it, and returns its recording. An existing project
/// is opened as it is, so its edits are not lost to a new guess.
pub fn import(video: &Path) -> Result<PathBuf> {
    let video = video.canonicalize().with_context(|| format!("reading {}", video.display()))?;
    let stem = video.file_stem().and_then(|s| s.to_str()).context("the video has no name")?;
    let dir = video.with_file_name(format!("{stem}.reel"));
    let path = dir.join("recording.json");
    if path.exists() {
        return Ok(path);
    }
    let probe = probe(&video)?;
    if probe.has_audio {
        eprintln!("reel: {} has sound, which a render leaves out", video.display());
    }
    let scale = if probe.width >= RETINA_WIDTH && probe.width % 2 == 0 && probe.height % 2 == 0 { 2 } else { 1 };
    let mut style = Style::default();
    style.cursor.show = false;
    let mut recording = Recording {
        video: video.clone(),
        size: [probe.width / scale, probe.height / scale],
        scale,
        fps: probe.fps,
        duration_ms: probe.duration_ms,
        events: Vec::new(),
        style,
        timeline: None,
        imported: true,
        cuts: Vec::new(),
    };
    recording.timeline = Some(analyze(&recording)?.validated(probe.duration_ms)?);
    fs::create_dir_all(&dir)?;
    fs::write(&path, serde_json::to_string_pretty(&recording)?)?;
    Ok(path)
}

struct Probe {
    width: u32,
    height: u32,
    fps: u32,
    duration_ms: f64,
    has_audio: bool,
}

fn probe(video: &Path) -> Result<Probe> {
    let output = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=codec_type,width,height,avg_frame_rate:format=duration", "-of", "json"])
        .arg(video)
        .output()
        .context("running ffprobe")?;
    if !output.status.success() {
        bail!("{} is not a video ffprobe can read: {}", video.display(), String::from_utf8_lossy(&output.stderr).trim());
    }
    parse_probe(&serde_json::from_slice(&output.stdout)?).with_context(|| format!("reading {}", video.display()))
}

fn parse_probe(probe: &Value) -> Result<Probe> {
    let streams = probe["streams"].as_array().context("no streams")?;
    let stream = streams.iter().find(|s| s["codec_type"] == "video").context("no video stream")?;
    let number = |key: &str| stream[key].as_u64().map(|n| n as u32).with_context(|| format!("no {key}"));
    let duration: f64 = probe["format"]["duration"].as_str().context("no duration")?.parse()?;
    Ok(Probe {
        width: number("width")?,
        height: number("height")?,
        fps: frame_rate(stream["avg_frame_rate"].as_str().unwrap_or("")),
        duration_ms: duration * 1_000.0,
        has_audio: streams.iter().any(|s| s["codec_type"] == "audio"),
    })
}

fn frame_rate(text: &str) -> u32 {
    let rate = text.split_once('/').and_then(|(n, d)| Some(n.parse::<f64>().ok()? / d.parse::<f64>().ok()?));
    rate.filter(|r| r.is_finite() && *r >= 1.0).map_or(30, |r| (r.round() as u32).min(60))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;

    const WIDTH: usize = 10;
    const STEP: f64 = 200.0;
    const FRAME: Size = Size { w: 1000.0, h: 1000.0 };

    fn still() -> Vec<u8> {
        vec![100; WIDTH * WIDTH]
    }

    fn with_spot(column: usize, row: usize) -> Vec<u8> {
        let mut frame = still();
        frame[row * WIDTH + column] = 200;
        frame
    }

    fn assert_region(got: Rect, want: Rect) {
        for (a, b) in [(got.x, want.x), (got.y, want.y), (got.w, want.w), (got.h, want.h)] {
            assert!((a - b).abs() < 1e-9, "{got:?} is not {want:?}");
        }
    }

    #[test]
    fn finds_nothing_in_a_still_video_or_in_noise() {
        let mut noisy = still();
        noisy[5] = 100 + CHANGED_LEVEL - 1;

        assert_eq!(bursts(&[still(), still(), noisy], WIDTH, STEP), []);
    }

    #[test]
    fn counts_a_change_of_exactly_the_noise_level() {
        let mut changed = still();
        changed[5] = 100 + CHANGED_LEVEL;

        assert_eq!(bursts(&[still(), changed], WIDTH, STEP).len(), 1);
    }

    #[test]
    fn marks_when_and_where_the_picture_changes() {
        let frames = [still(), still(), with_spot(2, 3), with_spot(4, 6), with_spot(4, 6)];

        let found = bursts(&frames, WIDTH, STEP);

        assert_eq!(found.len(), 1);
        assert_eq!((found[0].start_ms, found[0].end_ms), (200.0, 600.0));
        assert_region(found[0].region, Rect { x: 0.2, y: 0.3, w: 0.3, h: 0.4 });
    }

    #[test]
    fn looks_at_a_small_change_rather_than_the_scrim_before_it() {
        let dimmed = vec![60; WIDTH * WIDTH];
        let mut typed = dimmed.clone();
        typed[3 * WIDTH + 2] = 200;

        let found = bursts(&[still(), dimmed.clone(), typed.clone(), dimmed], WIDTH, STEP);

        assert_eq!(found.len(), 1);
        assert_region(found[0].region, Rect { x: 0.2, y: 0.3, w: 0.1, h: 0.1 });
    }

    #[test]
    fn keeps_a_wide_region_when_nothing_smaller_changes() {
        let dimmed = vec![60; WIDTH * WIDTH];

        let found = bursts(&[still(), dimmed.clone(), still()], WIDTH, STEP);

        assert_eq!(found[0].region, Rect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 });
    }

    #[test]
    fn splits_bursts_only_across_a_long_enough_gap() {
        let burst_after = |still_frames: usize| {
            let mut frames = vec![still(), with_spot(1, 1)];
            frames.extend(vec![with_spot(1, 1); still_frames]);
            frames.push(still());
            bursts(&frames, WIDTH, STEP).len()
        };

        assert_eq!(burst_after(5), 1, "exactly the gap");
        assert_eq!(burst_after(6), 2);
    }

    fn burst(start_ms: f64, end_ms: f64, region: Rect) -> Burst {
        Burst { start_ms, end_ms, region }
    }

    const SMALL: Rect = Rect { x: 0.1, y: 0.1, w: 0.2, h: 0.2 };
    const WHOLE: Rect = Rect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 };

    fn guess(bursts: &[Burst]) -> Timeline {
        timeline_from(bursts, 20_000.0, FRAME, &Style::default()).validated(20_000.0).unwrap()
    }

    #[test]
    fn speeds_up_the_stretches_where_nothing_happens() {
        let timeline = guess(&[burst(5_000.0, 6_000.0, WHOLE)]);

        assert_eq!(timeline.speeds.len(), 2);
        assert!(timeline.speeds[0].start_ms >= 0.0 && timeline.speeds[0].end_ms <= 5_000.0);
        assert!(timeline.speeds[1].start_ms >= 6_000.0 && timeline.speeds[1].end_ms <= 20_000.0);
    }

    #[test]
    fn leaves_short_pauses_at_real_speed() {
        assert_eq!(guess(&[burst(0.0, 1_000.0, WHOLE), burst(2_499.0, 20_000.0, WHOLE)]).speeds, []);
        assert_eq!(guess(&[burst(0.0, 1_000.0, WHOLE), burst(2_500.0, 20_000.0, WHOLE)]).speeds.len(), 1);
    }

    #[test]
    fn zooms_onto_a_change_in_part_of_the_frame() {
        let zooms = guess(&[burst(2_000.0, 3_000.0, SMALL)]).zooms;

        assert_eq!(zooms.len(), 1);
        assert_eq!(zooms[0].start_ms, 2_000.0);
        assert_eq!(zooms[0].end_ms, 3_000.0 + ZOOM_HOLD_MS);
        assert!(zooms[0].x < 500.0 && zooms[0].y < 500.0, "looking away from the change: {:?}", zooms[0]);
        assert_eq!(zooms[0].level, Style::default().zoom.level);
    }

    #[test]
    fn does_not_zoom_on_a_flash_or_on_most_of_the_frame() {
        assert_eq!(guess(&[burst(2_000.0, 2_399.0, SMALL)]).zooms, []);
        assert_eq!(guess(&[burst(2_000.0, 2_400.0, SMALL)]).zooms.len(), 1);
        let big = |side: f64| Rect { x: 0.0, y: 0.0, w: side, h: side };
        assert_eq!(guess(&[burst(2_000.0, 3_000.0, big(0.7))]).zooms, []);
        assert_eq!(guess(&[burst(2_000.0, 3_000.0, big(0.6))]).zooms.len(), 1);
    }

    #[test]
    fn never_overlaps_two_zooms_and_stops_at_the_end_of_the_video() {
        let zooms = guess(&[burst(2_000.0, 3_000.0, SMALL), burst(3_500.0, 4_500.0, SMALL), burst(19_000.0, 19_900.0, SMALL)]).zooms;

        assert_eq!(zooms.len(), 3);
        assert_eq!(zooms[1].start_ms, zooms[0].end_ms);
        assert_eq!(zooms[2].end_ms, 20_000.0);
    }

    #[test]
    fn reads_a_frame_rate_and_falls_back_when_it_cannot() {
        assert_eq!(frame_rate("30000/1001"), 30);
        assert_eq!(frame_rate("120/1"), 60);
        assert_eq!(frame_rate("0/0"), 30);
        assert_eq!(frame_rate(""), 30);
    }

    #[test]
    fn says_what_a_probe_is_missing() {
        assert!(parse_probe(&serde_json::json!({ "streams": [{ "codec_type": "audio" }], "format": { "duration": "1" } })).is_err());
        let found = parse_probe(&serde_json::json!({ "streams": [{ "codec_type": "video", "width": 8, "height": 6, "avg_frame_rate": "10/1" }, { "codec_type": "audio" }], "format": { "duration": "1.5" } })).unwrap();
        assert_eq!((found.width, found.height, found.fps, found.duration_ms, found.has_audio), (8, 6, 10, 1_500.0, true));
    }

    fn box_video(name: &str, width: u32) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("reel-import-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let video = dir.join("clip.mp4");
        let filter = format!("color=gray:s={width}x360:r=10:d=8[bg];color=white:s=40x40:r=10:d=8[box];[bg][box]overlay=x='20+20*t':y=20:enable='between(t,2,3)'[out]");
        let made = Command::new("ffmpeg").args(["-v", "error", "-y", "-filter_complex", &filter, "-map", "[out]", "-pix_fmt", "yuv420p"]).arg(&video).status().unwrap();
        assert!(made.success());
        video
    }

    #[test]
    fn makes_a_first_guess_for_a_real_video_and_opens_it_in_the_editor() {
        let path = import(&box_video("guess", 640)).unwrap();

        let recording = Recording::load(&path).unwrap();
        assert!(recording.imported);
        assert_eq!((recording.size, recording.scale, recording.fps), ([640, 360], 1, 10));
        assert!(!recording.style.cursor.show, "would draw a second cursor over the video's own");
        let timeline = recording.timeline.as_ref().unwrap();
        assert_eq!(timeline.clicks, []);
        assert_eq!(timeline.zooms.len(), 1, "{:?}", timeline.zooms);
        assert!(timeline.zooms[0].x < 320.0 && timeline.zooms[0].y < 180.0);
        assert!(timeline.zooms[0].start_ms >= 1_500.0 && timeline.zooms[0].start_ms <= 2_500.0, "{:?}", timeline.zooms[0]);
        assert!(timeline.speeds.iter().any(|s| s.start_ms >= 3_000.0 && s.end_ms <= 8_000.0), "{:?}", timeline.speeds);
        assert!(Editor::open(&path).is_ok());
    }

    #[test]
    fn keeps_the_edits_of_a_video_already_imported() {
        let video = box_video("keep", 640);
        let path = import(&video).unwrap();
        fs::write(&path, "edited").unwrap();

        assert_eq!(import(&video).unwrap(), path);
        assert_eq!(fs::read_to_string(&path).unwrap(), "edited");
    }

    #[test]
    fn treats_a_wide_even_video_as_retina_and_a_narrower_one_as_it_is() {
        let retina = Recording::load(&import(&box_video("retina", 2560)).unwrap()).unwrap();
        let plain = Recording::load(&import(&box_video("plain", 2558)).unwrap()).unwrap();

        assert_eq!((retina.size, retina.scale), ([1280, 180], 2));
        assert_eq!((plain.size, plain.scale), ([2558, 360], 1));
    }

    #[test]
    fn says_when_a_file_is_not_a_video() {
        let dir = std::env::temp_dir().join(format!("reel-import-bad-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let notes = dir.join("notes.txt");
        fs::write(&notes, "hello").unwrap();

        let error = import(&notes).unwrap_err();

        assert!(error.to_string().contains("notes.txt"), "{error}");
        assert!(import(&dir.join("missing.mp4")).is_err());
    }
}
