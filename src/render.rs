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
use crate::events::Pointer;
use crate::plan::{Cursor, Plan, Ripple, View};
use crate::style::{Aspect, Background, CursorLook, Fill, Rgb, RippleStyle, Style};

/// The output frame and where the video sits in it, in device pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub width: u32,
    pub height: u32,
    pub inner: Rect,
    /// Device pixels per CSS pixel.
    pub density: u32,
}

impl Layout {
    pub fn new(css_size: [u32; 2], density: u32, background: &Background) -> Layout {
        let [css_width, css_height] = css_size;
        let (css_w, css_h) = match background.aspect {
            Aspect::Auto => (css_width, css_height),
            Aspect::Ratio(w, h) if (w as u64 * css_height as u64) >= (h as u64 * css_width as u64) => {
                (even(css_height as f64 * w as f64 / h as f64), css_height)
            }
            Aspect::Ratio(w, h) => (css_width, even(css_width as f64 * h as f64 / w as f64)),
        };
        let (width, height) = (css_w * density, css_h * density);
        let (w, h) = (width as f32, height as f32);
        let padding = if background.fill == Fill::None { 0.0 } else { background.padding as f32 / 100.0 };
        let (room_w, room_h) = (w * (1.0 - 2.0 * padding), h * (1.0 - 2.0 * padding));
        let fit = (room_w / css_width as f32).min(room_h / css_height as f32);
        let (inner_w, inner_h) = (css_width as f32 * fit, css_height as f32 * fit);
        let inner = Rect::from_xywh((w - inner_w) / 2.0, (h - inner_h) / 2.0, inner_w, inner_h).expect("padding leaves room for the video");
        Layout { width, height, inner, density }
    }
}

fn even(value: f64) -> u32 {
    (value / 2.0).round() as u32 * 2
}

pub struct Compositor {
    style: Style,
    /// Frame pixels per CSS pixel: the plan and the style are in CSS pixels.
    density: f32,
    inner: Rect,
    background: Pixmap,
    video_mask: Mask,
    arrow: Shape,
    hand: Shape,
    beam: Shape,
}

impl Compositor {
    pub fn new(layout: Layout, style: &Style) -> Result<Compositor> {
        let Layout { width, height, inner, .. } = layout;
        let density = layout.density as f32;
        let backdrop = &style.background;
        let radius = if backdrop.fill == Fill::None { 0.0 } else { backdrop.corners as f32 / 100.0 * inner.width().min(inner.height()) };
        let window = rounded_rect(inner, radius);

        let mut background = fill_pixmap(&backdrop.fill, width, height)?;
        if backdrop.fill != Fill::None && backdrop.shadow > 0.0 && inner.width() < width as f32 {
            let alpha = (backdrop.shadow * 9.0 / 50.0).round() as u8;
            // A soft shadow from stacked translucent outlines: tiny-skia has no blur.
            for spread in (1..=14).rev() {
                let s = spread as f32 * 1.6 * density;
                let grown = Rect::from_xywh(inner.x() - s, inner.y() - s + 6.0 * density, inner.width() + 2.0 * s, inner.height() + 2.0 * s).unwrap();
                let mut shade = Paint::default();
                shade.set_color(Color::from_rgba8(0, 0, 0, alpha));
                shade.anti_alias = true;
                background.fill_path(&rounded_rect(grown, radius + s), &shade, FillRule::Winding, Transform::identity(), None);
            }
        }

        let mut video_mask = Mask::new(width, height).expect("frame size");
        video_mask.fill_path(&window, FillRule::Winding, true, Transform::identity());

        Ok(Compositor { style: style.clone(), density, inner, background, video_mask, arrow: arrow(), hand: hand(), beam: beam() })
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
            let Some(circle) = PathBuilder::from_circle(x, y, radius) else { continue };
            let clicks = &self.style.clicks;
            let mut paint = Paint { anti_alias: true, ..Paint::default() };
            match clicks.style {
                RippleStyle::Filled => {
                    paint.set_color(color(clicks.color, ((1.0 - progress) * 115.0) as u8));
                    out.fill_path(&circle, &paint, FillRule::Winding, Transform::identity(), Some(&self.video_mask));
                }
                RippleStyle::Outline => {
                    paint.set_color(color(clicks.color, ((1.0 - progress) * 230.0) as u8));
                    let stroke = Stroke { width: 3.0 * self.density * magnification.sqrt(), ..Stroke::default() };
                    out.stroke_path(&circle, &paint, &stroke, Transform::identity(), Some(&self.video_mask));
                }
            }
        }

        self.place_cursor(&mut out, view, cursor, 0.0, 0.0);
        out
    }

    /// Draws the cursor on `out`, whose top left corner is at (`left`, `top`) of the frame.
    fn place_cursor(&self, out: &mut Pixmap, view: View, cursor: Option<Cursor>, left: f32, top: f32) {
        let Some(cursor) = cursor.filter(|c| self.style.cursor.show && c.opacity > 0.0) else { return };
        let scale = self.inner.width() / view.w as f32;
        let (x, y) = (self.inner.x() + (cursor.x - view.x) as f32 * scale, self.inner.y() + (cursor.y - view.y) as f32 * scale);
        if !(self.inner.left()..=self.inner.right()).contains(&x) || !(self.inner.top()..=self.inner.bottom()).contains(&y) {
            return;
        }
        let size = CURSOR_SIZE / 20.0 * self.density * (scale / self.density).powf(0.6) * self.style.cursor.size as f32 * (1.0 - 0.18 * cursor.pressed as f32);
        self.draw_cursor(out, x - left, y - top, size, cursor.opacity as f32, cursor.pointer);
    }

    /// Averages the frame across the shots of one shutter. Only what moves is drawn more than once:
    /// compositing the whole video for each shot of a cursor move would be most of the render time.
    pub fn compose_blurred(&self, source: &Pixmap, shots: &[(View, Cursor)], ripples: &[Ripple]) -> Pixmap {
        let (first_view, first_cursor) = shots[0];
        if shots.iter().all(|(view, cursor)| *view == first_view && *cursor == first_cursor) {
            return self.compose(source, first_view, Some(first_cursor), ripples);
        }
        if shots.iter().any(|(view, _)| *view != first_view) {
            let frames = shots.iter().map(|(view, cursor)| self.compose(source, *view, Some(*cursor), ripples));
            return average(frames, shots.len() as u32);
        }
        let mut out = self.compose(source, first_view, None, ripples);
        let (left, top, right, bottom) = self.cursor_bounds(first_view, shots);
        let Some(area) = tiny_skia::IntRect::from_ltrb(left, top, right, bottom) else { return out };
        let Some(base) = out.clone_rect(area) else { return out };
        let crops = shots.iter().map(|(_, cursor)| {
            let mut crop = base.clone();
            self.place_cursor(&mut crop, first_view, Some(*cursor), left as f32, top as f32);
            crop
        });
        let trail = average(crops, shots.len() as u32);
        let stride = out.width() as usize * 4;
        for (row, pixels) in trail.data().chunks_exact(trail.width() as usize * 4).enumerate() {
            let start = (top as usize + row) * stride + left as usize * 4;
            out.data_mut()[start..start + pixels.len()].copy_from_slice(pixels);
        }
        out
    }

    fn cursor_bounds(&self, view: View, shots: &[(View, Cursor)]) -> (i32, i32, i32, i32) {
        let scale = self.inner.width() / view.w as f32;
        let reach = CURSOR_SIZE * self.density * (scale / self.density).powf(0.6) * self.style.cursor.size as f32 * 1.5;
        let (mut left, mut top, mut right, mut bottom) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for (_, cursor) in shots {
            let (x, y) = (self.inner.x() + (cursor.x - view.x) as f32 * scale, self.inner.y() + (cursor.y - view.y) as f32 * scale);
            (left, top, right, bottom) = (left.min(x), top.min(y), right.max(x), bottom.max(y));
        }
        let (width, height) = (self.background.width() as i32, self.background.height() as i32);
        (
            ((left - reach) as i32).clamp(0, width),
            ((top - reach) as i32).clamp(0, height),
            ((right + reach) as i32).clamp(0, width),
            ((bottom + reach) as i32).clamp(0, height),
        )
    }

    fn draw_cursor(&self, out: &mut Pixmap, x: f32, y: f32, size: f32, opacity: f32, pointer: Pointer) {
        let at = Transform::from_translate(x, y).pre_scale(size, size);
        let paint = |r: u8, g: u8, b: u8, a: f32| {
            let mut paint = Paint { anti_alias: true, ..Paint::default() };
            paint.set_color(Color::from_rgba8(r, g, b, (a * opacity * 255.0) as u8));
            paint
        };
        let (body, line) = match self.style.cursor.style {
            CursorLook::WhiteArrow => (paint(255, 255, 255, 1.0), paint(20, 20, 24, 1.0)),
            CursorLook::BlackArrow => (paint(16, 16, 18, 1.0), paint(255, 255, 255, 1.0)),
            CursorLook::Dot => {
                let Some(dot) = PathBuilder::from_circle(0.0, 0.0, 7.0) else { return };
                out.fill_path(&dot, &paint(16, 16, 18, 0.72), FillRule::Winding, at, None);
                out.stroke_path(&dot, &paint(255, 255, 255, 1.0), &Stroke { width: 1.4, ..Stroke::default() }, at, None);
                return;
            }
        };
        let silhouette = match pointer {
            Pointer::Arrow => &self.arrow,
            Pointer::Hand => &self.hand,
            Pointer::Text => {
                out.stroke_path(&self.beam, &line, &Stroke { width: 3.4, ..Stroke::default() }, at, None);
                out.stroke_path(&self.beam, &body, &Stroke { width: 1.5, ..Stroke::default() }, at, None);
                return;
            }
        };
        out.fill_path(silhouette, &paint(0, 0, 0, 0.27), FillRule::Winding, at.post_translate(1.5 * size, 2.5 * size), None);
        out.fill_path(silhouette, &body, FillRule::Winding, at, None);
        out.stroke_path(silhouette, &line, &Stroke { width: 1.4, ..Stroke::default() }, at, None);
    }
}

const CURSOR_SIZE: f32 = 26.0;

fn average(frames: impl Iterator<Item = Pixmap>, count: u32) -> Pixmap {
    let mut sums: Vec<u32> = Vec::new();
    let mut last = None;
    for frame in frames {
        if sums.is_empty() {
            sums = vec![0; frame.data().len()];
        }
        for (sum, byte) in sums.iter_mut().zip(frame.data()) {
            *sum += *byte as u32;
        }
        last = Some(frame);
    }
    let mut out = last.expect("a shutter has at least one shot");
    for (byte, sum) in out.data_mut().iter_mut().zip(sums) {
        *byte = ((sum + count / 2) / count) as u8;
    }
    out
}

fn fill_pixmap(fill: &Fill, width: u32, height: u32) -> Result<Pixmap> {
    let mut pixmap = Pixmap::new(width, height).expect("frame size");
    let (w, h) = (width as f32, height as f32);
    match fill {
        Fill::None => pixmap.fill(Color::BLACK),
        Fill::Color(rgb) => pixmap.fill(color(*rgb, 255)),
        Fill::Gradient(from, to) => {
            let gradient = LinearGradient::new(
                Point::from_xy(0.0, 0.0),
                Point::from_xy(w, h),
                vec![GradientStop::new(0.0, color(*from, 255)), GradientStop::new(1.0, color(*to, 255))],
                SpreadMode::Pad,
                Transform::identity(),
            );
            let paint = Paint { shader: gradient.unwrap_or(tiny_skia::Shader::SolidColor(color(*from, 255))), ..Paint::default() };
            pixmap.fill_rect(Rect::from_xywh(0.0, 0.0, w, h).unwrap(), &paint, Transform::identity(), None);
        }
        Fill::Image(path) => {
            let rgb = read_image(path, width, height)?;
            for (pixel, source) in pixmap.data_mut().chunks_exact_mut(4).zip(rgb.chunks_exact(3)) {
                pixel[..3].copy_from_slice(source);
                pixel[3] = 255;
            }
        }
    }
    Ok(pixmap)
}

/// Decodes an image of any format ffmpeg reads, scaled to cover the frame and cropped to it.
fn read_image(path: &Path, width: u32, height: u32) -> Result<Vec<u8>> {
    let cover = format!("scale={width}:{height}:force_original_aspect_ratio=increase:flags=lanczos,crop={width}:{height}");
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-vf", &cover, "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .output()
        .context("starting ffmpeg")?;
    if !output.status.success() || output.stdout.len() != (width * height * 3) as usize {
        bail!("could not read the background image {}: {}", path.display(), String::from_utf8_lossy(&output.stderr));
    }
    Ok(output.stdout)
}

fn color(Rgb([r, g, b]): Rgb, alpha: u8) -> Color {
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

/// A pointing hand with its fingertip at the origin.
fn hand() -> Shape {
    let mut path = PathBuilder::new();
    path.move_to(-1.7, 1.5);
    path.quad_to(-1.7, 0.0, 0.0, 0.0);
    path.quad_to(1.7, 0.0, 1.7, 1.5);
    path.line_to(1.7, 6.6);
    path.quad_to(3.2, 5.7, 4.3, 6.9);
    path.quad_to(5.9, 6.2, 6.9, 7.6);
    path.quad_to(8.6, 7.1, 9.3, 8.8);
    path.line_to(9.3, 13.2);
    path.quad_to(9.3, 17.8, 5.6, 18.6);
    path.line_to(0.6, 18.6);
    path.quad_to(-1.4, 18.6, -2.6, 16.6);
    path.line_to(-6.2, 11.2);
    path.quad_to(-7.0, 9.6, -5.6, 9.0);
    path.quad_to(-4.4, 8.6, -3.2, 10.0);
    path.line_to(-1.7, 11.8);
    path.close();
    path.finish().expect("the hand is a valid path")
}

/// A text cursor centered on the origin.
fn beam() -> Shape {
    let mut path = PathBuilder::new();
    path.move_to(-3.0, -9.0);
    path.quad_to(0.0, -9.0, 0.0, -7.0);
    path.quad_to(0.0, -9.0, 3.0, -9.0);
    path.move_to(0.0, -7.0);
    path.line_to(0.0, 7.0);
    path.move_to(-3.0, 9.0);
    path.quad_to(0.0, 9.0, 0.0, 7.0);
    path.quad_to(0.0, 9.0, 3.0, 9.0);
    path.finish().expect("the text cursor is a valid path")
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
    let (source_width, source_height) = (css_width * recording.scale, css_height * recording.scale);
    let layout = Layout::new(recording.size, recording.scale, &recording.style.background);
    let (width, height) = (layout.width, layout.height);
    let fps = recording.fps as f64;
    let mut decoder = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&recording.video)
        .args(["-vf", &format!("fps={fps},scale={source_width}:{source_height}:flags=lanczos"), "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .stdout(Stdio::piped())
        .spawn()
        .context("starting the ffmpeg decoder")?;
    let mut encoder = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgba", "-s", &format!("{width}x{height}"), "-r", &recording.fps.to_string(), "-i", "-"])
        .args(["-vf", &format!("scale={}:{}:flags=lanczos", width / recording.scale, height / recording.scale)])
        .args(["-c:v", "libx264", "-preset", "medium", "-crf", "18", "-pix_fmt", "yuv420p", "-movflags", "+faststart"])
        .arg(output)
        .stdin(Stdio::piped())
        .spawn()
        .context("starting the ffmpeg encoder")?;
    let mut frames = decoder.stdout.take().expect("piped stdout");
    let mut sink = encoder.stdin.take().expect("piped stdin");

    let compositor = Compositor::new(layout, &recording.style)?;
    let workers = thread::available_parallelism().map_or(1, |n| n.get());
    let mut buffer = vec![0u8; (source_width * source_height * 4) as usize];
    let mut current: Option<Arc<Pixmap>> = None;
    let mut decoded: i64 = -1;
    let total = (plan.time.output_duration_ms() / 1_000.0 * fps).ceil() as i64;
    let (shots, shutter) = recording.style.motion_blur.shutter();
    let frame_ms = 1_000.0 / fps;
    let mut batch: Vec<(Arc<Pixmap>, Vec<f64>)> = Vec::with_capacity(workers);
    for index in 0..total {
        let output_ms = index as f64 * frame_ms;
        let source_ms = plan.time.source_ms(output_ms);
        let wanted = (source_ms * fps / 1_000.0).floor() as i64;
        while decoded < wanted {
            match frames.read_exact(&mut buffer) {
                Ok(()) => {
                    current = Pixmap::from_vec(buffer.clone(), tiny_skia::IntSize::from_wh(source_width, source_height).unwrap()).map(Arc::new);
                    decoded += 1;
                }
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }
        }
        let Some(source) = current.as_ref() else { bail!("the raw video has no frames") };
        let times = (0..shots).map(|shot| plan.time.source_ms(output_ms + shot as f64 / shots as f64 * shutter * frame_ms)).collect();
        batch.push((source.clone(), times));
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

fn compose_all(compositor: &Compositor, plan: &Plan, batch: &[(Arc<Pixmap>, Vec<f64>)]) -> Vec<Pixmap> {
    thread::scope(|scope| {
        let workers: Vec<_> = batch
            .iter()
            .map(|(source, times)| {
                scope.spawn(|| {
                    let shots: Vec<(View, Cursor)> = times.iter().map(|t| (plan.camera_at(*t), plan.cursor_at(*t))).collect();
                    compositor.compose_blurred(source, &shots, &plan.ripples_at(times[0]))
                })
            })
            .collect();
        workers.into_iter().map(|w| w.join().expect("a compositing thread panicked")).collect()
    })
}

pub fn plan_for(recording: &Recording) -> Plan {
    Plan::new(&recording.resolved_timeline(), recording.duration_ms, recording.frame(), &recording.style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::{Clicks, CursorStyle};
    use tiny_skia::Color;

    const RED: Rgb = Rgb([220, 30, 30]);
    const BLUE: Rgb = Rgb([30, 30, 220]);
    const BLACK: Rgb = Rgb([0, 0, 0]);
    const GREEN: Rgb = Rgb([0, 200, 0]);
    const WHITE: Rgb = Rgb([255, 255, 255]);

    fn flat(background: Rgb) -> Style {
        Style { background: Background { fill: Fill::Color(background), padding: 0.0, corners: 0.0, ..Background::default() }, ..Style::default() }
    }

    fn compositor(size: [u32; 2], density: u32, style: &Style) -> Compositor {
        Compositor::new(Layout::new(size, density, &style.background), style).unwrap()
    }

    fn halves() -> Pixmap {
        let mut source = Pixmap::new(200, 100).unwrap();
        source.fill(color(RED, 255));
        let blue = tiny_skia::Rect::from_xywh(100.0, 0.0, 100.0, 100.0).unwrap();
        let mut paint = tiny_skia::Paint::default();
        paint.set_color(color(BLUE, 255));
        source.fill_rect(blue, &paint, tiny_skia::Transform::identity(), None);
        source
    }

    fn rgb(pixmap: &Pixmap, x: u32, y: u32) -> Rgb {
        let p = pixmap.pixel(x, y).unwrap();
        Rgb([p.red(), p.green(), p.blue()])
    }

    fn near(a: Rgb, b: Rgb) -> bool {
        a.0.iter().zip(b.0).all(|(x, y)| (*x as i32 - y as i32).abs() < 12)
    }

    const FULL: View = View { x: 0.0, y: 0.0, w: 200.0, h: 100.0 };

    #[test]
    fn shows_the_whole_source_when_the_camera_is_out() {
        let out = compositor([200, 100], 1, &flat(BLACK)).compose(&halves(), FULL, None, &[]);

        assert!(near(rgb(&out, 20, 50), RED));
        assert!(near(rgb(&out, 180, 50), BLUE));
    }

    #[test]
    fn fills_the_frame_with_the_part_the_camera_zooms_onto() {
        let right_half = View { x: 100.0, y: 25.0, w: 100.0, h: 50.0 };

        let out = compositor([200, 100], 1, &flat(BLACK)).compose(&halves(), right_half, None, &[]);

        assert!(near(rgb(&out, 20, 50), BLUE), "left edge is {:?}", rgb(&out, 20, 50));
        assert!(near(rgb(&out, 180, 50), BLUE));
    }

    #[test]
    fn frames_the_video_on_the_background() {
        let mut style = flat(GREEN);
        style.background.padding = 10.0;

        let out = compositor([400, 200], 1, &style).compose(&halves(), FULL, None, &[]);

        assert!(near(rgb(&out, 2, 2), GREEN), "corner is {:?}", rgb(&out, 2, 2));
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

        let out = compositor([200, 100], 2, &flat(BLACK)).compose(&doubled(&halves()), right_half, None, &[]);

        assert_eq!((out.width(), out.height()), (400, 200));
        assert!(near(rgb(&out, 40, 100), BLUE), "left edge is {:?}", rgb(&out, 40, 100));
        assert!(near(rgb(&out, 360, 100), BLUE));
    }

    #[test]
    fn draws_the_cursor_at_its_css_position_on_a_2x_frame() {
        let mut white = Pixmap::new(400, 200).unwrap();
        white.fill(Color::WHITE);
        let cursor = Cursor { x: 60.0, y: 30.0, pressed: 0.0, opacity: 1.0, pointer: Pointer::Arrow };

        let out = compositor([200, 100], 2, &flat(BLACK)).compose(&white, View { x: 0.0, y: 0.0, w: 200.0, h: 100.0 }, Some(cursor), &[]);

        let dark = |xs: std::ops::Range<u32>, ys: std::ops::Range<u32>| {
            let out = &out;
            xs.flat_map(move |x| ys.clone().map(move |y| (x, y))).filter(|&(x, y)| rgb(out, x, y).0[0] < 128).count()
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
        let recording = Recording { video, size: [320, 180], scale: 2, fps: 10, duration_ms: 1_000.0, events: vec![], style: Style::default(), timeline: None };
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
    fn plans_a_saved_timeline_instead_of_the_automatic_pass() {
        let click = crate::events::Event::Click { at_ms: 500.0, target: crate::events::Rect { x: 10.0, y: 10.0, w: 20.0, h: 20.0 }, frame: crate::events::Rect { x: 10.0, y: 10.0, w: 20.0, h: 20.0 }, pointer: Pointer::Arrow };
        let mut recording = Recording { video: "raw.mp4".into(), size: [320, 180], scale: 1, fps: 10, duration_ms: 4_000.0, events: vec![click], style: Style::default(), timeline: None };
        assert!(plan_for(&recording).camera_at(1_500.0).w < 320.0, "the automatic pass did not zoom");

        recording.timeline = Some(crate::timeline::Timeline::default());

        assert_eq!(plan_for(&recording).camera_at(1_500.0).w, 320.0);
    }

    fn backdrop(fill: Fill, padding: f64, shadow: f64, corners: f64) -> Style {
        Style { background: Background { fill, padding, shadow, corners, ..Background::default() }, ..Style::default() }
    }

    #[test]
    fn keeps_todays_layout_by_default() {
        let layout = Layout::new([1920, 1080], 2, &Background::default());

        assert_eq!((layout.width, layout.height), (3840, 2160));
        assert_eq!((layout.inner.x(), layout.inner.y()), (192.0, 108.0));
        assert_eq!((layout.inner.width(), layout.inner.height()), (3456.0, 1944.0));
    }

    #[test]
    fn widens_the_frame_for_a_wider_aspect_and_centers_the_video() {
        let background = Background { aspect: Aspect::Ratio(2, 1), padding: 0.0, ..Background::default() };

        let layout = Layout::new([100, 100], 1, &background);

        assert_eq!((layout.width, layout.height), (200, 100));
        assert_eq!((layout.inner.x(), layout.inner.width()), (50.0, 100.0));
    }

    #[test]
    fn heightens_the_frame_for_a_taller_aspect_with_even_sides() {
        let background = Background { aspect: Aspect::Ratio(9, 16), padding: 0.0, ..Background::default() };

        let layout = Layout::new([1000, 1000], 1, &background);

        assert_eq!((layout.width, layout.height), (1000, 1778));
        assert_eq!(layout.inner.width(), 1000.0);
        assert_eq!(layout.inner.y(), 389.0);
    }

    #[test]
    fn leaves_no_room_around_the_video_without_a_background() {
        let background = Background { fill: Fill::None, padding: 12.0, ..Background::default() };

        let layout = Layout::new([200, 100], 1, &background);

        assert_eq!((layout.inner.x(), layout.inner.width()), (0.0, 200.0));
    }

    #[test]
    fn draws_neither_corners_nor_a_shadow_without_a_background() {
        let style = backdrop(Fill::None, 12.0, 100.0, 40.0);

        let out = compositor([200, 100], 1, &style).compose(&halves(), FULL, None, &[]);

        assert!(near(rgb(&out, 0, 0), RED), "corner is {:?}", rgb(&out, 0, 0));
    }

    #[test]
    fn rounds_the_corners_by_a_percent_of_the_short_side() {
        let style = backdrop(Fill::Color(GREEN), 0.0, 0.0, 30.0);

        let out = compositor([200, 100], 1, &style).compose(&halves(), FULL, None, &[]);

        assert!(near(rgb(&out, 1, 1), GREEN), "corner is {:?}", rgb(&out, 1, 1));
        assert!(near(rgb(&out, 40, 1), RED), "top edge is {:?}", rgb(&out, 40, 1));
    }

    #[test]
    fn darkens_the_backdrop_more_with_a_stronger_shadow() {
        let shadow_at = |strength| {
            let style = backdrop(Fill::Color(WHITE), 20.0, strength, 0.0);
            rgb(&compositor([400, 200], 1, &style).compose(&halves(), FULL, None, &[]), 200, 165).0[0]
        };

        assert_eq!(shadow_at(0.0), 255);
        assert!(shadow_at(50.0) < 200, "{}", shadow_at(50.0));
        assert!(shadow_at(100.0) < shadow_at(50.0));
    }

    #[test]
    fn fills_the_backdrop_with_an_image() {
        let dir = std::env::temp_dir().join(format!("reel-wallpaper-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let image = dir.join("wall.png");
        let made = Command::new("ffmpeg").args(["-v", "error", "-y", "-f", "lavfi", "-i", "color=#00c800:s=64x64", "-frames:v", "1"]).arg(&image).status().unwrap();
        assert!(made.success());
        let style = backdrop(Fill::Image(image), 10.0, 0.0, 0.0);

        let out = compositor([200, 100], 1, &style).compose(&halves(), FULL, None, &[]);

        assert!(near(rgb(&out, 2, 2), GREEN), "corner is {:?}", rgb(&out, 2, 2));
        assert!(near(rgb(&out, 50, 50), RED));
    }

    #[test]
    fn says_which_background_image_it_could_not_read() {
        let style = backdrop(Fill::Image("/nonexistent/wall.png".into()), 10.0, 0.0, 0.0);

        let Err(error) = Compositor::new(Layout::new([200, 100], 1, &style.background), &style) else { panic!("read a missing image") };

        assert!(error.to_string().contains("/nonexistent/wall.png"), "{error}");
    }

    #[test]
    fn renders_a_frame_of_the_asked_aspect() {
        let dir = std::env::temp_dir().join(format!("reel-render-aspect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let video = dir.join("raw.mp4");
        let made = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "color=white:s=640x360:r=10:d=1", "-pix_fmt", "yuv420p"])
            .arg(&video)
            .status()
            .unwrap();
        assert!(made.success());
        let style = Style { background: Background { aspect: Aspect::Ratio(1, 1), ..Background::default() }, ..Style::default() };
        let recording = Recording { video, size: [320, 180], scale: 2, fps: 10, duration_ms: 1_000.0, events: vec![], style, timeline: None };
        let output = dir.join("out.mp4");

        render(&recording, &plan_for(&recording), &output).unwrap();

        let probe = Command::new("ffprobe").args(["-v", "error", "-show_entries", "stream=width,height", "-of", "csv=p=0"]).arg(&output).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&probe.stdout).trim(), "320,320");
    }

    #[test]
    fn draws_the_cursor_where_the_plan_puts_it() {
        let mut white = Pixmap::new(200, 100).unwrap();
        white.fill(Color::WHITE);
        let cursor = Cursor { x: 60.0, y: 30.0, pressed: 0.0, opacity: 1.0, pointer: Pointer::Arrow };

        let out = compositor([200, 100], 1, &flat(BLACK)).compose(&white, FULL, Some(cursor), &[]);

        let dark = (60..76).flat_map(|x| (30..56).map(move |y| (x, y))).filter(|&(x, y)| rgb(&out, x, y).0[0] < 128).count();
        assert!(dark > 10, "only {dark} dark pixels near the cursor");
        assert_eq!(rgb(&out, 150, 80), WHITE);
    }

    fn white_frame() -> Pixmap {
        let mut white = Pixmap::new(200, 100).unwrap();
        white.fill(Color::WHITE);
        white
    }

    fn cursor_frame(cursor: CursorStyle, at: Cursor) -> Pixmap {
        let style = Style { cursor, ..flat(BLACK) };
        compositor([200, 100], 1, &style).compose(&white_frame(), FULL, Some(at), &[])
    }

    const SPOT: Cursor = Cursor { x: 60.0, y: 30.0, pressed: 0.0, opacity: 1.0, pointer: Pointer::Arrow };

    fn dark_pixels(out: &Pixmap) -> usize {
        (0..200).flat_map(|x| (0..100).map(move |y| (x, y))).filter(|&(x, y)| rgb(out, x, y).0[0] < 128).count()
    }

    #[test]
    fn draws_nothing_when_the_cursor_is_turned_off_or_faded_out() {
        let off = cursor_frame(CursorStyle { show: false, ..CursorStyle::default() }, SPOT);
        let faded = cursor_frame(CursorStyle::default(), Cursor { opacity: 0.0, ..SPOT });

        assert_eq!(dark_pixels(&off), 0);
        assert_eq!(dark_pixels(&faded), 0);
    }

    #[test]
    fn scales_the_cursor_with_its_size() {
        let normal = dark_pixels(&cursor_frame(CursorStyle::default(), SPOT));
        let double = dark_pixels(&cursor_frame(CursorStyle { size: 2.0, ..CursorStyle::default() }, SPOT));

        assert!(double > normal * 3 / 2, "{normal} then {double}");
    }

    #[test]
    fn fills_a_black_arrow_dark_and_a_white_arrow_light() {
        let inside = (62, 42);
        let white = cursor_frame(CursorStyle::default(), SPOT);
        let black = cursor_frame(CursorStyle { style: CursorLook::BlackArrow, ..CursorStyle::default() }, SPOT);

        assert!(rgb(&white, inside.0, inside.1).0[0] > 200, "{:?}", rgb(&white, inside.0, inside.1));
        assert!(rgb(&black, inside.0, inside.1).0[0] < 60, "{:?}", rgb(&black, inside.0, inside.1));
    }

    #[test]
    fn centers_a_dot_on_the_cursor_position() {
        let out = cursor_frame(CursorStyle { style: CursorLook::Dot, ..CursorStyle::default() }, SPOT);

        assert!(rgb(&out, 60, 30).0[0] < 128, "center is {:?}", rgb(&out, 60, 30));
        assert!(rgb(&out, 56, 30).0[0] < 128, "left of center is {:?}", rgb(&out, 56, 30));
        assert_eq!(rgb(&out, 80, 30), WHITE);
    }

    #[test]
    fn colours_a_ripple_and_leaves_an_outline_hollow() {
        let ripple = Ripple { x: 100.0, y: 50.0, progress: 0.2 };
        let frame = |clicks: Clicks| {
            let style = Style { clicks, ..flat(BLACK) };
            compositor([200, 100], 1, &style).compose(&white_frame(), FULL, None, &[ripple])
        };
        let blue = Rgb([0, 0, 255]);

        let filled = frame(Clicks { color: blue, ..Clicks::default() });
        let outline = frame(Clicks { color: blue, style: RippleStyle::Outline, ..Clicks::default() });

        let center = rgb(&filled, 100, 50).0;
        assert!(center[2] > center[0] && center[0] < 255, "filled center is {center:?}");
        assert_eq!(rgb(&outline, 100, 50), WHITE, "the outline filled its center");
        let radius = 18;
        let edge = rgb(&outline, 100 + radius, 50).0;
        assert!(edge[0] < 200, "the outline has no edge at {radius}: {edge:?}");
    }

    fn at_x(x: f64) -> (View, Cursor) {
        (FULL, Cursor { x, ..SPOT })
    }

    #[test]
    fn leaves_a_frame_with_nothing_moving_exactly_as_it_is() {
        let c = compositor([200, 100], 1, &flat(BLACK));
        let still = c.compose_blurred(&white_frame(), &[at_x(60.0), at_x(60.0), at_x(60.0)], &[]);

        assert_eq!(still.data(), c.compose(&white_frame(), FULL, Some(at_x(60.0).1), &[]).data());
    }

    #[test]
    fn smears_a_moving_cursor_and_leaves_the_rest_alone() {
        let c = compositor([200, 100], 1, &flat(BLACK));
        let shots = [at_x(40.0), at_x(55.0), at_x(70.0), at_x(85.0)];

        let out = c.compose_blurred(&white_frame(), &shots, &[]);

        let partial = (30..120).flat_map(|x| (30..60).map(move |y| (x, y))).filter(|&(x, y)| (20..235).contains(&rgb(&out, x, y).0[0])).count();
        let sharp = c.compose(&white_frame(), FULL, Some(at_x(40.0).1), &[]);
        let sharp_partial = (30..120).flat_map(|x| (30..60).map(move |y| (x, y))).filter(|&(x, y)| (20..235).contains(&rgb(&sharp, x, y).0[0])).count();
        assert!(partial > sharp_partial * 2, "{partial} blended pixels against {sharp_partial}");
        assert_eq!(rgb(&out, 170, 80), WHITE);
    }

    #[test]
    fn draws_a_blurred_cursor_in_the_same_place_as_averaging_whole_frames() {
        let c = compositor([200, 100], 1, &flat(BLACK));
        let shots = [at_x(40.0), at_x(60.0), at_x(190.0)];

        let out = c.compose_blurred(&white_frame(), &shots, &[]);

        let whole = average(shots.iter().map(|(view, cursor)| c.compose(&white_frame(), *view, Some(*cursor), &[])), 3);
        assert_eq!(out.data(), whole.data());
    }

    #[test]
    fn blends_the_views_of_a_moving_camera() {
        let c = compositor([200, 100], 1, &flat(BLACK));
        let hidden = Cursor { opacity: 0.0, ..SPOT };
        let shots = [(FULL, hidden), (View { x: 100.0, y: 25.0, w: 100.0, h: 50.0 }, hidden)];

        let out = c.compose_blurred(&halves(), &shots, &[]);

        let left = rgb(&out, 20, 50).0;
        assert!(left[0] > 80 && left[2] > 80, "left edge is {left:?}, not red and blue averaged");
    }

    #[test]
    fn draws_a_different_shape_for_each_pointer() {
        let draw = |pointer| cursor_frame(CursorStyle::default(), Cursor { pointer, ..SPOT });
        let (arrow, hand, text) = (draw(Pointer::Arrow), draw(Pointer::Hand), draw(Pointer::Text));

        assert_ne!(arrow.data(), hand.data());
        assert_ne!(arrow.data(), text.data());
        assert!((56..=64).any(|x| rgb(&text, x, 30).0[0] < 128), "no text cursor on its position");
        assert!(dark_pixels(&text) < dark_pixels(&hand), "{} against {}", dark_pixels(&text), dark_pixels(&hand));
    }

    #[test]
    fn keeps_a_dot_whatever_the_page_asks_for() {
        let dot = CursorStyle { style: CursorLook::Dot, ..CursorStyle::default() };

        let plain = cursor_frame(dot.clone(), SPOT);
        let over_a_link = cursor_frame(dot, Cursor { pointer: Pointer::Hand, ..SPOT });

        assert_eq!(plain.data(), over_a_link.data());
    }

    #[test]
    fn leaves_out_a_cursor_the_camera_has_left_behind() {
        let mut style = flat(GREEN);
        style.background.padding = 20.0;
        let c = compositor([200, 100], 1, &style);
        let right_half = View { x: 100.0, y: 25.0, w: 100.0, h: 50.0 };
        let behind = Cursor { x: 85.0, y: 50.0, ..SPOT };

        let with_cursor = c.compose(&white_frame(), right_half, Some(behind), &[]);

        assert_eq!(with_cursor.data(), c.compose(&white_frame(), right_half, None, &[]).data());
    }

    #[test]
    fn tints_the_spot_of_a_click() {
        let mut white = Pixmap::new(200, 100).unwrap();
        white.fill(Color::WHITE);
        let ripple = Ripple { x: 100.0, y: 50.0, progress: 0.3 };

        let out = compositor([200, 100], 1, &flat(BLACK)).compose(&white, FULL, None, &[ripple]);

        assert_ne!(rgb(&out, 100, 50), WHITE);
        assert_eq!(rgb(&out, 10, 10), WHITE);
    }
}
