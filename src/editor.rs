use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tiny_skia::Pixmap;

use crate::capture::Recording;
use crate::events::Pointer;
use crate::motion::analyze;
use crate::plan::Plan;
use crate::render::{exact_frame, read_source_frame, render_with_progress, snapped_to_frame, Compositor, Layout, SPRITE_SCALE, SPRITE_SIZE, SPRITE_TIP};
use crate::server::{Request, Response};
use crate::style::Style;
use crate::timeline::Timeline;

const PAGE: &str = include_str!("editor.html");
const SAMPLE_MS: f64 = 1_000.0 / 30.0;
const STRIP_FRAMES: u32 = 40;
const STRIP_HEIGHT: u32 = 54;

/// One recording open for editing. Owns its state: every change goes through `handle`, reaches
/// disk before it is answered, and a new generation tells the page its images are stale.
pub struct Editor {
    path: PathBuf,
    preview: PathBuf,
    strip: PathBuf,
    render_output: PathBuf,
    working: Mutex<Working>,
    source_frame: Mutex<Option<(i64, Arc<Pixmap>)>>,
    job: Arc<Mutex<RenderJob>>,
}

struct Working {
    recording: Recording,
    generation: u64,
    plan: Arc<Plan>,
    /// At one device pixel per CSS pixel, for what the page draws while playing.
    quick: Arc<Compositor>,
    /// At the take's density, for exact frames. Built on first use: a 4K shadow takes a moment.
    full: Option<Arc<Compositor>>,
}

impl Working {
    fn new(recording: Recording, generation: u64) -> Result<Working> {
        let plan = Plan::new(&recording.resolved_timeline(), recording.duration_ms, recording.frame(), &recording.style);
        let quick = Compositor::new(Layout::new(recording.size, 1, &recording.style.background), &recording.style)?;
        Ok(Working { recording, generation, plan: Arc::new(plan), quick: Arc::new(quick), full: None })
    }

    fn full(&mut self) -> Result<Arc<Compositor>> {
        if self.full.is_none() {
            let recording = &self.recording;
            let layout = Layout::new(recording.size, recording.scale, &recording.style.background);
            self.full = Some(Arc::new(Compositor::new(layout, &recording.style)?));
        }
        Ok(self.full.clone().expect("just built"))
    }

    /// Everything the page needs to draw the take: the plan sampled here, so its easing and timing
    /// live only in Rust and the page interpolates numbers.
    fn project(&self) -> Value {
        let recording = &self.recording;
        let inner = self.quick.inner();
        let layout = Layout::new(recording.size, 1, &recording.style.background);
        let mut rows = Vec::new();
        let count = (recording.duration_ms / SAMPLE_MS).ceil() as usize + 1;
        for i in 0..count {
            let t = i as f64 * SAMPLE_MS;
            let view = self.plan.camera_at(t);
            let cursor = self.plan.cursor_at(t);
            let pointer = match cursor.pointer {
                Pointer::Arrow => 0.0,
                Pointer::Hand => 1.0,
                Pointer::Text => 2.0,
            };
            let size = self.quick.cursor_scale(view, cursor.pressed) as f64;
            rows.extend([view.x, view.y, view.w, cursor.x, cursor.y, cursor.pressed, cursor.opacity, pointer, size].map(|v| (v * 100.0).round() / 100.0));
        }
        json!({
            "generation": self.generation,
            "name": take_name(&recording.video),
            "size": recording.size,
            "scale": recording.scale,
            "fps": recording.fps,
            "duration_ms": recording.duration_ms,
            "output_ms": self.plan.time.output_duration_ms(),
            "imported": recording.imported,
            "edited": recording.timeline.is_some(),
            "style": recording.style,
            "timeline": recording.resolved_timeline(),
            "layout": { "width": layout.width, "height": layout.height, "x": inner.x(), "y": inner.y(), "w": inner.width(), "h": inner.height(), "radius": self.quick.radius() },
            "sample_ms": SAMPLE_MS,
            "rows": rows,
            "sprite": { "size": SPRITE_SIZE, "tip": SPRITE_TIP, "scale": SPRITE_SCALE },
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    style: Style,
    /// Absent until someone changes the timeline: the automatic pass keeps following the style.
    timeline: Option<Timeline>,
}

#[derive(Default)]
struct RenderJob {
    running: bool,
    progress: f64,
    error: Option<String>,
    finished: bool,
}

impl Editor {
    pub fn open(path: &Path) -> Result<Editor> {
        let recording = Recording::load(path)?;
        let path = path.canonicalize()?;
        let dir = path.parent().context("the recording has no folder")?.to_path_buf();
        let (preview, strip) = (dir.join("preview.mp4"), dir.join("strip.jpg"));
        if !is_up_to_date(&preview, &recording.video) || !is_up_to_date(&strip, &recording.video) {
            eprintln!("reel: preparing a preview of {}", recording.video.display());
            make_preview(&recording, &preview, &strip)?;
        }
        let working = Working::new(recording, 1)?;
        Ok(Editor { render_output: dir.join("render.mp4"), path, preview, strip, working: Mutex::new(working), source_frame: Mutex::new(None), job: Arc::default() })
    }

    pub fn handle(&self, request: &Request) -> Response {
        match self.route(request) {
            Ok(response) => response,
            Err(error) => Response::error(400, &format!("{error:#}")),
        }
    }

    fn route(&self, request: &Request) -> Result<Response> {
        Ok(match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/") => Response::bytes("text/html; charset=utf-8", PAGE.as_bytes().to_vec()),
            ("GET", "/api/project") => json_response(self.lock().project()),
            ("POST", "/api/edit") => {
                let edit: Edit = serde_json::from_slice(&request.body).context("reading the edit")?;
                let duration_ms = self.lock().recording.duration_ms;
                let timeline = edit.timeline.map(|t| t.validated(duration_ms)).transpose()?;
                let style = edit.style.validated()?;
                self.apply(|recording| {
                    recording.style = style;
                    recording.timeline = timeline;
                })?
            }
            ("POST", "/api/auto") => {
                let recording = self.lock().recording.clone();
                let guess = if recording.imported { Some(analyze(&recording)?.validated(recording.duration_ms)?) } else { None };
                self.apply(|recording| recording.timeline = guess)?
            }
            ("POST", "/api/render") => self.start_render()?,
            ("GET", "/api/render") => {
                let job = self.job.lock().expect("render job");
                json_response(json!({ "running": job.running, "progress": job.progress, "finished": job.finished, "error": job.error, "output": self.render_output }))
            }
            ("GET", "/video") => Response::file("video/mp4", self.preview.clone()),
            ("GET", "/strip.jpg") => Response::file("image/jpeg", self.strip.clone()),
            ("GET", "/output.mp4") if self.render_output.exists() => Response::file("video/mp4", self.render_output.clone()),
            ("GET", "/background.png") => Response::bytes("image/png", self.lock().quick.background_png()?),
            ("GET", "/sprite.png") => {
                let pointer = match request.query.get("pointer").map(String::as_str) {
                    Some("hand") => Pointer::Hand,
                    Some("text") => Pointer::Text,
                    _ => Pointer::Arrow,
                };
                Response::bytes("image/png", self.lock().quick.sprite_png(pointer)?)
            }
            ("GET", "/frame.png") => {
                let source_ms: f64 = request.query.get("ms").context("which frame?")?.parse().context("reading the frame's time")?;
                Response::bytes("image/png", self.exact_png(source_ms)?)
            }
            _ => Response::error(404, "no such page"),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Working> {
        self.working.lock().expect("working state")
    }

    /// Builds the new state first, so a change that cannot be drawn leaves the old one and the file alone.
    fn apply(&self, change: impl FnOnce(&mut Recording)) -> Result<Response> {
        let mut working = self.lock();
        let mut recording = working.recording.clone();
        change(&mut recording);
        let next = Working::new(recording, working.generation + 1)?;
        write_atomically(&self.path, serde_json::to_string_pretty(&next.recording)?.as_bytes())?;
        *working = next;
        Ok(json_response(working.project()))
    }

    fn exact_png(&self, source_ms: f64) -> Result<Vec<u8>> {
        let (recording, plan, compositor) = {
            let mut working = self.lock();
            let compositor = working.full()?;
            (working.recording.clone(), working.plan.clone(), compositor)
        };
        let (output_ms, source_ms) = snapped_to_frame(&recording, &plan, source_ms);
        let index = (source_ms * recording.fps as f64 / 1_000.0).floor() as i64;
        let cached = self.source_frame.lock().expect("source frame").clone();
        let source = match cached {
            Some((held, frame)) if held == index => frame,
            _ => {
                let frame = Arc::new(read_source_frame(&recording, source_ms)?);
                *self.source_frame.lock().expect("source frame") = Some((index, frame.clone()));
                frame
            }
        };
        exact_frame(&recording, &plan, &compositor, &source, output_ms).encode_png().context("encoding the frame")
    }

    fn start_render(&self) -> Result<Response> {
        let (recording, plan) = {
            let working = self.lock();
            (working.recording.clone(), working.plan.clone())
        };
        {
            let mut job = self.job.lock().expect("render job");
            if job.running {
                bail!("a render is already running");
            }
            *job = RenderJob { running: true, ..RenderJob::default() };
        }
        let (job, output) = (self.job.clone(), self.render_output.clone());
        thread::spawn(move || {
            let progress = {
                let job = job.clone();
                move |done: f64| job.lock().expect("render job").progress = done
            };
            let result = render_with_progress(&recording, &plan, &output, &progress);
            let mut job = job.lock().expect("render job");
            job.running = false;
            job.finished = result.is_ok();
            job.error = result.err().map(|e| format!("{e:#}"));
        });
        Ok(json_response(json!({ "running": true })))
    }
}

/// A take is `<name>/raw.mp4`, so the folder names it better than the file.
fn take_name(video: &Path) -> String {
    let stem = video.file_stem().and_then(|s| s.to_str()).unwrap_or("take");
    if stem != "raw" {
        return stem.to_string();
    }
    video.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or(stem).to_string()
}

fn json_response(value: Value) -> Response {
    Response::bytes("application/json", value.to_string().into_bytes())
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes).with_context(|| format!("writing {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("replacing {}", path.display()))
}

fn is_up_to_date(derived: &Path, source: &Path) -> bool {
    let modified = |path: &Path| fs::metadata(path).and_then(|m| m.modified());
    matches!((modified(derived), modified(source)), (Ok(derived), Ok(source)) if derived >= source)
}

/// A take is 4:4:4 at twice the size, which browsers will not decode or not decode fast enough. The
/// preview is what a browser plays and seeks in; a short keyframe interval keeps seeking quick.
fn make_preview(recording: &Recording, preview: &Path, strip: &Path) -> Result<()> {
    let [width, height] = recording.size;
    let made = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(&recording.video)
        .args(["-an", "-vf", &format!("scale={width}:{height}:flags=lanczos"), "-c:v", "libx264", "-preset", "veryfast", "-crf", "20", "-pix_fmt", "yuv420p"])
        .args(["-g", &(recording.fps / 2).max(1).to_string(), "-movflags", "+faststart"])
        .arg(preview)
        .status()
        .context("starting ffmpeg")?;
    if !made.success() {
        bail!("ffmpeg could not make a preview of {}", recording.video.display());
    }
    let every = recording.duration_ms / 1_000.0 / STRIP_FRAMES as f64;
    let made = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(preview)
        .args(["-vf", &format!("fps=1/{every:.4},scale=-2:{STRIP_HEIGHT},tile={STRIP_FRAMES}x1"), "-frames:v", "1", "-q:v", "4"])
        .arg(strip)
        .status()
        .context("starting ffmpeg")?;
    if !made.success() {
        bail!("ffmpeg could not make a film strip of {}", recording.video.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Event, Rect};
    use crate::server::Body;
    use crate::style::Fill;
    use crate::timeline::ZoomSpan;
    use std::time::Duration;

    fn take(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("reel-editor-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let video = dir.join("raw.mp4");
        let made = Command::new("ffmpeg").args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=s=640x360:r=10:d=2", "-pix_fmt", "yuv420p"]).arg(&video).status().unwrap();
        assert!(made.success());
        let button = Rect { x: 100.0, y: 80.0, w: 40.0, h: 20.0 };
        let click = Event::Click { at_ms: 900.0, target: button, frame: button, pointer: Pointer::Hand };
        let recording = Recording { video, size: [320, 180], scale: 2, fps: 10, duration_ms: 2_000.0, events: vec![click], style: Style::default(), timeline: None, imported: false };
        let path = dir.join("recording.json");
        fs::write(&path, serde_json::to_string(&recording).unwrap()).unwrap();
        path
    }

    fn get(editor: &Editor, target: &str) -> Response {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let query = query.split('&').filter_map(|p| p.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect();
        editor.handle(&Request { method: "GET".into(), path: path.into(), query, ..Request::default() })
    }

    fn post(editor: &Editor, path: &str, body: Value) -> Response {
        editor.handle(&Request { method: "POST".into(), path: path.into(), body: body.to_string().into_bytes(), ..Request::default() })
    }

    fn bytes(response: &Response) -> &[u8] {
        match &response.body {
            Body::Bytes(bytes) => bytes,
            Body::File(path) => panic!("expected bytes, got {}", path.display()),
        }
    }

    fn json_of(response: &Response) -> Value {
        serde_json::from_slice(bytes(response)).unwrap()
    }

    fn zoom() -> ZoomSpan {
        ZoomSpan { start_ms: 500.0, end_ms: 1_500.0, x: 160.0, y: 90.0, level: 2.0 }
    }

    #[test]
    fn opens_an_unedited_take_on_its_automatic_timeline_without_touching_it() {
        let path = take("open");
        let before = fs::read_to_string(&path).unwrap();
        let editor = Editor::open(&path).unwrap();

        let project = json_of(&get(&editor, "/api/project"));

        assert_eq!(project["edited"], false);
        assert_eq!(project["timeline"]["clicks"].as_array().unwrap().len(), 1);
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn names_a_take_by_its_folder_and_any_other_video_by_its_file() {
        assert_eq!(take_name(Path::new("/out/domains/raw.mp4")), "domains");
        assert_eq!(take_name(Path::new("/Movies/launch.mov")), "launch");
    }

    #[test]
    fn samples_the_plan_across_the_whole_take() {
        let editor = Editor::open(&take("rows")).unwrap();

        let project = json_of(&get(&editor, "/api/project"));

        let rows = project["rows"].as_array().unwrap();
        assert_eq!(rows.len() % 9, 0);
        assert_eq!(rows.len() / 9, (2_000.0_f64 / SAMPLE_MS).ceil() as usize + 1);
        assert_eq!(rows[2], 320.0, "starts on the whole frame");
        assert_eq!(project["layout"]["width"], 320);
    }

    #[test]
    fn saves_an_edit_so_a_later_load_reproduces_it() {
        let path = take("save");
        let editor = Editor::open(&path).unwrap();
        let style = Style { motion_blur: crate::style::MotionBlur::Low, ..Style::default() };
        let timeline = Timeline { zooms: vec![zoom()], ..Timeline::default() };

        let reply = post(&editor, "/api/edit", json!({ "style": style, "timeline": timeline }));

        assert_eq!(reply.status, 200);
        assert_eq!(json_of(&reply)["generation"], 2);
        let saved = Recording::load(&path).unwrap();
        assert_eq!(saved.timeline, Some(timeline));
        assert_eq!(saved.style, style);
        assert_eq!(saved.events.len(), 1, "lost the event log");
    }

    #[test]
    fn keeps_following_the_automatic_pass_when_only_the_style_changes() {
        let path = take("style");
        let editor = Editor::open(&path).unwrap();
        let style = Style { zoom: crate::style::Zoom { level: 1.1, ..Default::default() }, ..Style::default() };

        let project = json_of(&post(&editor, "/api/edit", json!({ "style": style, "timeline": null })));

        assert_eq!(project["edited"], false);
        assert!((project["timeline"]["zooms"][0]["level"].as_f64().unwrap() - 1.1).abs() < 0.001, "{}", project["timeline"]["zooms"]);
        assert!(Recording::load(&path).unwrap().timeline.is_none());
    }

    #[test]
    fn refuses_an_edit_it_cannot_use_and_keeps_what_it_had() {
        let path = take("refuse");
        let editor = Editor::open(&path).unwrap();
        let before = fs::read_to_string(&path).unwrap();
        let backwards = Timeline { zooms: vec![ZoomSpan { start_ms: 900.0, end_ms: 100.0, ..zoom() }], ..Timeline::default() };
        let wall = Style { background: crate::style::Background { fill: Fill::Image("/nonexistent/wall.png".into()), ..Default::default() }, ..Style::default() };
        let bad_range = Style { zoom: crate::style::Zoom { level: 99.0, ..Default::default() }, ..Style::default() };

        for body in [json!({ "style": Style::default(), "timeline": backwards }), json!({ "style": wall, "timeline": null }), json!({ "style": bad_range, "timeline": null }), json!({ "style": {}, "timeline": null, "extra": 1 }), json!("nonsense")] {
            let reply = post(&editor, "/api/edit", body.clone());
            assert_eq!(reply.status, 400, "accepted {body}");
            assert!(json_of(&reply)["error"].is_string());
        }

        assert_eq!(fs::read_to_string(&path).unwrap(), before);
        assert_eq!(json_of(&get(&editor, "/api/project"))["generation"], 1);
    }

    #[test]
    fn goes_back_to_the_automatic_timeline_on_request() {
        let path = take("auto");
        let editor = Editor::open(&path).unwrap();
        let timeline = Timeline { zooms: vec![zoom()], ..Timeline::default() };
        post(&editor, "/api/edit", json!({ "style": Style::default(), "timeline": timeline }));

        let project = json_of(&post(&editor, "/api/auto", json!({})));

        assert_eq!(project["edited"], false);
        assert_eq!(project["timeline"]["clicks"].as_array().unwrap().len(), 1);
        assert!(Recording::load(&path).unwrap().timeline.is_none());
    }

    #[test]
    fn draws_an_exact_frame_at_the_takes_density_with_the_latest_edit() {
        let editor = Editor::open(&take("frame")).unwrap();
        let frame = |editor: &Editor| Pixmap::decode_png(bytes(&get(editor, "/frame.png?ms=100"))).unwrap();
        let first = frame(&editor);
        assert_eq!((first.width(), first.height()), (640, 360));

        let green = Style { background: crate::style::Background { fill: Fill::Color(crate::style::Rgb([0, 200, 0])), shadow: 0.0, ..Default::default() }, ..Style::default() };
        post(&editor, "/api/edit", json!({ "style": green, "timeline": null }));

        let corner = frame(&editor).pixel(1, 1).unwrap();
        assert_eq!((corner.red(), corner.green(), corner.blue()), (0, 200, 0));
    }

    #[test]
    fn says_what_is_wrong_with_a_frame_request() {
        let editor = Editor::open(&take("badframe")).unwrap();

        assert_eq!(get(&editor, "/frame.png").status, 400);
        assert_eq!(get(&editor, "/frame.png?ms=soon").status, 400);
    }

    #[test]
    fn serves_the_page_the_preview_the_strip_and_the_drawing_parts() {
        let editor = Editor::open(&take("assets")).unwrap();

        assert!(String::from_utf8_lossy(bytes(&get(&editor, "/"))).contains("<canvas"));
        for (target, kind) in [("/video", "video/mp4"), ("/strip.jpg", "image/jpeg")] {
            let reply = get(&editor, target);
            let Body::File(path) = &reply.body else { panic!("{target} is not a file") };
            assert!(fs::metadata(path).unwrap().len() > 0, "{target} is empty");
            assert_eq!(reply.content_type, kind);
        }
        let background = Pixmap::decode_png(bytes(&get(&editor, "/background.png"))).unwrap();
        assert_eq!((background.width(), background.height()), (320, 180));
        assert_ne!(bytes(&get(&editor, "/sprite.png?pointer=hand")), bytes(&get(&editor, "/sprite.png")));
        assert_eq!(get(&editor, "/nothing").status, 404);
        assert_eq!(get(&editor, "/output.mp4").status, 404, "offered a render before there was one");
    }

    #[test]
    fn reuses_a_preview_newer_than_the_take() {
        let path = take("reuse");
        drop(Editor::open(&path).unwrap());
        let preview = path.with_file_name("preview.mp4");
        fs::write(&preview, "kept").unwrap();

        drop(Editor::open(&path).unwrap());

        assert_eq!(fs::read_to_string(&preview).unwrap(), "kept");
    }

    #[test]
    fn renders_the_edited_take_in_the_background_and_reports_the_end() {
        let path = take("render");
        let editor = Editor::open(&path).unwrap();

        assert_eq!(post(&editor, "/api/render", json!({})).status, 200);
        let mut status = json_of(&get(&editor, "/api/render"));
        for _ in 0..200 {
            if status["running"] == false {
                break;
            }
            thread::sleep(Duration::from_millis(50));
            status = json_of(&get(&editor, "/api/render"));
        }

        assert_eq!(status["finished"], true, "{status}");
        assert_eq!(status["progress"], 1.0);
        assert!(status["error"].is_null());
        assert_eq!(get(&editor, "/output.mp4").status, 200);
    }
}
