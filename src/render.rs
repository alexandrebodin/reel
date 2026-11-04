use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;

use anyhow::{bail, Context, Result};
use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop, LinearGradient, Mask, Paint, Path as Shape, PathBuilder, Pixmap,
    PixmapPaint, Point, Rect, SpreadMode, Stroke, Transform,
};

use crate::capture::Recording;
use crate::plan::{Cursor, Plan, Ripple, Size, View};

pub type Rgb = [u8; 3];

#[derive(Clone, Debug)]
pub struct Look {
    /// Diagonal gradient behind the video, top left to bottom right.
    pub background: [Rgb; 2],
    /// Share of each side left for the background.
    pub padding: f32,
    pub radius: f32,
    pub accent: Rgb,
    pub cursor_size: f32,
}

impl Default for Look {
    fn default() -> Look {
        Look { background: [[45, 27, 105], [226, 88, 31]], padding: 0.05, radius: 18.0, accent: [255, 106, 61], cursor_size: 26.0 }
    }
}

pub struct Compositor {
    style: Look,
    /// Frame pixels per CSS pixel: the plan and the style are in CSS pixels.
    density: f32,
    inner: Rect,
    background: Pixmap,
    video_mask: Mask,
    arrow: Shape,
}

impl Compositor {
    pub fn new(width: u32, height: u32, density: u32, style: &Look) -> Compositor {
        let density = density as f32;
        let (w, h) = (width as f32, height as f32);
        let inner = Rect::from_xywh(w * style.padding, h * style.padding, w * (1.0 - 2.0 * style.padding), h * (1.0 - 2.0 * style.padding))
            .expect("padding leaves room for the video");
        let window = rounded_rect(inner, style.radius * density);

        let mut background = Pixmap::new(width, height).expect("frame size");
        let [from, to] = style.background;
        let gradient = LinearGradient::new(
            Point::from_xy(0.0, 0.0),
            Point::from_xy(w, h),
            vec![GradientStop::new(0.0, color(from, 255)), GradientStop::new(1.0, color(to, 255))],
            SpreadMode::Pad,
            Transform::identity(),
        );
        let paint = Paint { shader: gradient.unwrap_or(tiny_skia::Shader::SolidColor(color(from, 255))), ..Paint::default() };
        background.fill_rect(Rect::from_xywh(0.0, 0.0, w, h).unwrap(), &paint, Transform::identity(), None);
        if style.padding > 0.0 {
            // A soft shadow from stacked translucent outlines: tiny-skia has no blur.
            for spread in (1..=14).rev() {
                let s = spread as f32 * 1.6 * density;
                let grown = Rect::from_xywh(inner.x() - s, inner.y() - s + 6.0 * density, inner.width() + 2.0 * s, inner.height() + 2.0 * s).unwrap();
                let mut shade = Paint::default();
                shade.set_color(Color::from_rgba8(0, 0, 0, 9));
                shade.anti_alias = true;
                background.fill_path(&rounded_rect(grown, style.radius * density + s), &shade, FillRule::Winding, Transform::identity(), None);
            }
        }

        let mut video_mask = Mask::new(width, height).expect("frame size");
        video_mask.fill_path(&window, FillRule::Winding, true, Transform::identity());

        Compositor { style: style.clone(), density, inner, background, video_mask, arrow: arrow() }
    }

    pub fn compose(&self, source: &Pixmap, view: View, cursor: Option<Cursor>, ripples: &[Ripple]) -> Pixmap {
        let mut out = self.background.clone();
        let scale = self.inner.width() / view.w as f32;
        let place = |x: f64, y: f64| {
            (self.inner.x() + (x - view.x) as f32 * scale, self.inner.y() + (y - view.y) as f32 * scale)
        };
        let magnification = scale / self.density;
        let to_frame = Transform::from_translate(self.inner.x(), self.inner.y())
            .pre_scale(magnification, magnification)
            .pre_translate(-view.x as f32 * self.density, -view.y as f32 * self.density);
        let paint = PixmapPaint { quality: FilterQuality::Bicubic, ..PixmapPaint::default() };
        out.draw_pixmap(0, 0, source.as_ref(), &paint, to_frame, Some(&self.video_mask));

        for ripple in ripples {
            let (x, y) = place(ripple.x, ripple.y);
            let progress = ripple.progress as f32;
            let radius = (12.0 + 30.0 * progress) * self.density * magnification.sqrt();
            if let Some(circle) = PathBuilder::from_circle(x, y, radius) {
                let mut fill = Paint { anti_alias: true, ..Paint::default() };
                fill.set_color(color(self.style.accent, ((1.0 - progress) * 115.0) as u8));
                out.fill_path(&circle, &fill, FillRule::Winding, Transform::identity(), Some(&self.video_mask));
            }
        }

        if let Some(cursor) = cursor {
            let (x, y) = place(cursor.x, cursor.y);
            let size = self.style.cursor_size / 20.0 * self.density * magnification.powf(0.6) * (1.0 - 0.18 * cursor.pressed as f32);
            let at = Transform::from_translate(x, y).pre_scale(size, size);
            let mut shadow = Paint { anti_alias: true, ..Paint::default() };
            shadow.set_color(Color::from_rgba8(0, 0, 0, 70));
            out.fill_path(&self.arrow, &shadow, FillRule::Winding, at.post_translate(1.5 * size, 2.5 * size), None);
            let mut body = Paint { anti_alias: true, ..Paint::default() };
            body.set_color(Color::WHITE);
            out.fill_path(&self.arrow, &body, FillRule::Winding, at, None);
            let mut outline = Paint { anti_alias: true, ..Paint::default() };
            outline.set_color(Color::from_rgba8(20, 20, 24, 255));
            out.stroke_path(&self.arrow, &outline, &Stroke { width: 1.4, ..Stroke::default() }, at, None);
        }
        out
    }
}

fn color([r, g, b]: Rgb, alpha: u8) -> Color {
    Color::from_rgba8(r, g, b, alpha)
}

fn rounded_rect(rect: Rect, radius: f32) -> Shape {
    let r = radius.min(rect.width() / 2.0).min(rect.height() / 2.0).max(0.0);
    let (left, top, right, bottom) = (rect.left(), rect.top(), rect.right(), rect.bottom());
    let mut path = PathBuilder::new();
    path.move_to(left + r, top);
    path.line_to(right - r, top);
    path.quad_to(right, top, right, top + r);
    path.line_to(right, bottom - r);
    path.quad_to(right, bottom, right - r, bottom);
    path.line_to(left + r, bottom);
    path.quad_to(left, bottom, left, bottom - r);
    path.line_to(left, top + r);
    path.quad_to(left, top, left + r, top);
    path.close();
    path.finish().expect("a rectangle is a valid path")
}

/// A pointer 20 units tall with its tip at the origin.
fn arrow() -> Shape {
    let points = [(0.0, 0.0), (0.0, 16.5), (4.2, 12.6), (7.0, 19.0), (9.6, 17.9), (6.9, 11.6), (12.2, 11.6)];
    let mut path = PathBuilder::new();
    path.move_to(points[0].0, points[0].1);
    for (x, y) in &points[1..] {
        path.line_to(*x, *y);
    }
    path.close();
    path.finish().expect("the arrow is a valid path")
}

pub fn video_duration_ms(video: &Path) -> Result<f64> {
    let output = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(video)
        .output()
        .context("running ffprobe")?;
    let seconds: f64 = String::from_utf8_lossy(&output.stdout).trim().parse().context("reading the video duration")?;
    Ok(seconds * 1_000.0)
}

/// Decodes the raw take frame by frame, composites each output frame at the capture's density and
/// streams it to an H.264 encoder, which downsamples to the demo's size.
pub fn render(recording: &Recording, plan: &Plan, output: &Path) -> Result<()> {
    let [css_width, css_height] = recording.size;
    let (width, height) = (css_width * recording.scale, css_height * recording.scale);
    let fps = recording.fps as f64;
    let mut decoder = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&recording.video)
        .args(["-vf", &format!("fps={fps},scale={width}:{height}:flags=lanczos"), "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .stdout(Stdio::piped())
        .spawn()
        .context("starting the ffmpeg decoder")?;
    let mut encoder = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgba", "-s", &format!("{width}x{height}"), "-r", &recording.fps.to_string(), "-i", "-"])
        .args(["-vf", &format!("scale={css_width}:{css_height}:flags=lanczos")])
        .args(["-c:v", "libx264", "-preset", "medium", "-crf", "18", "-pix_fmt", "yuv420p", "-movflags", "+faststart"])
        .arg(output)
        .stdin(Stdio::piped())
        .spawn()
        .context("starting the ffmpeg encoder")?;
    let mut frames = decoder.stdout.take().expect("piped stdout");
    let mut sink = encoder.stdin.take().expect("piped stdin");

    let compositor = Compositor::new(width, height, recording.scale, &Look::default());
    let workers = thread::available_parallelism().map_or(1, |n| n.get());
    let mut buffer = vec![0u8; (width * height * 4) as usize];
    let mut current: Option<Arc<Pixmap>> = None;
    let mut decoded: i64 = -1;
    let total = (plan.time.output_duration_ms() / 1_000.0 * fps).ceil() as i64;
    let mut batch: Vec<(Arc<Pixmap>, f64)> = Vec::with_capacity(workers);
    for index in 0..total {
        let source_ms = plan.time.source_ms(index as f64 * 1_000.0 / fps);
        let wanted = (source_ms * fps / 1_000.0).floor() as i64;
        while decoded < wanted {
            match frames.read_exact(&mut buffer) {
                Ok(()) => {
                    current = Pixmap::from_vec(buffer.clone(), tiny_skia::IntSize::from_wh(width, height).unwrap()).map(Arc::new);
                    decoded += 1;
                }
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }
        }
        let Some(source) = current.as_ref() else { bail!("the raw video has no frames") };
        batch.push((source.clone(), source_ms));
        if batch.len() == workers || index == total - 1 {
            for frame in compose_all(&compositor, plan, &batch) {
                sink.write_all(frame.data())?;
            }
            batch.clear();
        }
    }
    drop(sink);
    drop(frames);
    let _ = decoder.wait();
    if !encoder.wait()?.success() {
        bail!("ffmpeg could not encode {}", output.display());
    }
    Ok(())
}

fn compose_all(compositor: &Compositor, plan: &Plan, batch: &[(Arc<Pixmap>, f64)]) -> Vec<Pixmap> {
    thread::scope(|scope| {
        let workers: Vec<_> = batch
            .iter()
            .map(|(source, at_ms)| {
                scope.spawn(|| compositor.compose(source, plan.camera_at(*at_ms), Some(plan.cursor_at(*at_ms)), &plan.ripples_at(*at_ms)))
            })
            .collect();
        workers.into_iter().map(|w| w.join().expect("a compositing thread panicked")).collect()
    })
}

pub fn plan_for(recording: &Recording) -> Plan {
    let size = Size { w: recording.size[0] as f64, h: recording.size[1] as f64 };
    Plan::new(&recording.events, recording.duration_ms, size, &recording.style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Style;
    use tiny_skia::Color;

    const RED: Rgb = [220, 30, 30];
    const BLUE: Rgb = [30, 30, 220];

    fn flat(background: Rgb) -> Look {
        Look { background: [background, background], padding: 0.0, radius: 0.0, accent: [255, 106, 61], cursor_size: 26.0 }
    }

    fn halves() -> Pixmap {
        let mut source = Pixmap::new(200, 100).unwrap();
        source.fill(Color::from_rgba8(RED[0], RED[1], RED[2], 255));
        let blue = tiny_skia::Rect::from_xywh(100.0, 0.0, 100.0, 100.0).unwrap();
        let mut paint = tiny_skia::Paint::default();
        paint.set_color_rgba8(BLUE[0], BLUE[1], BLUE[2], 255);
        source.fill_rect(blue, &paint, tiny_skia::Transform::identity(), None);
        source
    }

    fn rgb(pixmap: &Pixmap, x: u32, y: u32) -> Rgb {
        let p = pixmap.pixel(x, y).unwrap();
        [p.red(), p.green(), p.blue()]
    }

    fn near(a: Rgb, b: Rgb) -> bool {
        a.iter().zip(b).all(|(x, y)| (*x as i32 - y as i32).abs() < 12)
    }

    const FULL: View = View { x: 0.0, y: 0.0, w: 200.0, h: 100.0 };

    #[test]
    fn shows_the_whole_source_when_the_camera_is_out() {
        let out = Compositor::new(200, 100, 1, &flat([0, 0, 0])).compose(&halves(), FULL, None, &[]);

        assert!(near(rgb(&out, 20, 50), RED));
        assert!(near(rgb(&out, 180, 50), BLUE));
    }

    #[test]
    fn fills_the_frame_with_the_part_the_camera_zooms_onto() {
        let right_half = View { x: 100.0, y: 25.0, w: 100.0, h: 50.0 };

        let out = Compositor::new(200, 100, 1, &flat([0, 0, 0])).compose(&halves(), right_half, None, &[]);

        assert!(near(rgb(&out, 20, 50), BLUE), "left edge is {:?}", rgb(&out, 20, 50));
        assert!(near(rgb(&out, 180, 50), BLUE));
    }

    #[test]
    fn frames_the_video_on_the_background() {
        let style = Look { padding: 0.1, ..flat([0, 200, 0]) };

        let out = Compositor::new(400, 200, 1, &style).compose(&halves(), FULL, None, &[]);

        assert!(near(rgb(&out, 2, 2), [0, 200, 0]), "corner is {:?}", rgb(&out, 2, 2));
        assert!(near(rgb(&out, 100, 100), RED));
    }

    fn doubled(source: &Pixmap) -> Pixmap {
        let mut big = Pixmap::new(source.width() * 2, source.height() * 2).unwrap();
        big.draw_pixmap(0, 0, source.as_ref(), &PixmapPaint::default(), Transform::from_scale(2.0, 2.0), None);
        big
    }

    #[test]
    fn keeps_a_2x_capture_in_place_when_the_plan_is_in_css_pixels() {
        let right_half = View { x: 100.0, y: 25.0, w: 100.0, h: 50.0 };

        let out = Compositor::new(400, 200, 2, &flat([0, 0, 0])).compose(&doubled(&halves()), right_half, None, &[]);

        assert_eq!((out.width(), out.height()), (400, 200));
        assert!(near(rgb(&out, 40, 100), BLUE), "left edge is {:?}", rgb(&out, 40, 100));
        assert!(near(rgb(&out, 360, 100), BLUE));
    }

    #[test]
    fn draws_the_cursor_at_its_css_position_on_a_2x_frame() {
        let mut white = Pixmap::new(400, 200).unwrap();
        white.fill(Color::WHITE);
        let cursor = Cursor { x: 60.0, y: 30.0, pressed: 0.0 };

        let out = Compositor::new(400, 200, 2, &flat([0, 0, 0])).compose(&white, View { x: 0.0, y: 0.0, w: 200.0, h: 100.0 }, Some(cursor), &[]);

        let dark = |xs: std::ops::Range<u32>, ys: std::ops::Range<u32>| {
            let out = &out;
            xs.flat_map(move |x| ys.clone().map(move |y| (x, y))).filter(|&(x, y)| rgb(out, x, y)[0] < 128).count()
        };
        assert!(dark(120..150, 60..110) > 20, "no cursor at (120, 60)");
        assert_eq!(dark(0..112, 0..200), 0, "cursor drawn left of its position");
    }

    #[test]
    fn renders_a_2x_capture_down_to_the_demos_size() {
        let dir = std::env::temp_dir().join(format!("reel-render-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let video = dir.join("raw.mp4");
        let made = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "color=white:s=640x360:r=10:d=1", "-pix_fmt", "yuv420p"])
            .arg(&video)
            .status()
            .unwrap();
        assert!(made.success());
        let recording = Recording { video, size: [320, 180], scale: 2, fps: 10, duration_ms: 1_000.0, events: vec![], style: Style::default() };
        let output = dir.join("out.mp4");

        render(&recording, &plan_for(&recording), &output).unwrap();

        let probe = Command::new("ffprobe")
            .args(["-v", "error", "-show_entries", "stream=width,height", "-of", "csv=p=0"])
            .arg(&output)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&probe.stdout).trim(), "320,180");
    }

    #[test]
    fn draws_the_cursor_where_the_plan_puts_it() {
        let mut white = Pixmap::new(200, 100).unwrap();
        white.fill(Color::WHITE);
        let cursor = Cursor { x: 60.0, y: 30.0, pressed: 0.0 };

        let out = Compositor::new(200, 100, 1, &flat([0, 0, 0])).compose(&white, FULL, Some(cursor), &[]);

        let dark = (60..76).flat_map(|x| (30..56).map(move |y| (x, y))).filter(|&(x, y)| rgb(&out, x, y)[0] < 128).count();
        assert!(dark > 10, "only {dark} dark pixels near the cursor");
        assert_eq!(rgb(&out, 150, 80), [255, 255, 255]);
    }

    #[test]
    fn tints_the_spot_of_a_click() {
        let mut white = Pixmap::new(200, 100).unwrap();
        white.fill(Color::WHITE);
        let ripple = Ripple { x: 100.0, y: 50.0, progress: 0.3 };

        let out = Compositor::new(200, 100, 1, &flat([0, 0, 0])).compose(&white, FULL, None, &[ripple]);

        assert_ne!(rgb(&out, 100, 50), [255, 255, 255]);
        assert_eq!(rgb(&out, 10, 10), [255, 255, 255]);
    }
}
