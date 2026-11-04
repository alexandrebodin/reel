use std::fs;
use std::io::Write;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Message, WebSocket};

/// Records the page from Chrome's own screencast. `agent-browser record` cannot be sharp: it asks
/// for JPEG frames at quality 80 and, under emulated device scales, gets 1x frames it upscales.
/// PNG frames from a browser launched at the device scale keep every pixel a screenshot has.
pub struct Screencast {
    stop: Arc<AtomicBool>,
    reader: JoinHandle<Result<Vec<f64>>>,
    frames: PathBuf,
    video: PathBuf,
    fps: u32,
    started_at: f64,
}

/// A script to run in the page, in this document and every later one, that reports through a binding.
pub struct PageListener {
    pub binding: String,
    pub script: String,
    pub signals: mpsc::Sender<String>,
}

impl Screencast {
    pub fn start(cdp_url: &str, page_url: &str, size: [u32; 2], scale: u32, video: &Path, fps: u32) -> Result<Screencast> {
        Screencast::start_with_listener(cdp_url, page_url, size, scale, video, fps, None)
    }

    pub fn start_with_listener(cdp_url: &str, page_url: &str, size: [u32; 2], scale: u32, video: &Path, fps: u32, listener: Option<PageListener>) -> Result<Screencast> {
        let frames = video.with_extension("frames");
        let _ = fs::remove_dir_all(&frames);
        fs::create_dir_all(&frames)?;

        let mut devtools = Devtools::connect(cdp_url)?;
        let targets = devtools.call("Target.getTargets", json!({}), None)?;
        let target = page_target(&targets, page_url).ok_or_else(|| anyhow!("no browser tab shows {page_url}"))?;
        let attached = devtools.call("Target.attachToTarget", json!({ "targetId": target, "flatten": true }), None)?;
        let session = attached["sessionId"].as_str().context("attaching to the page")?.to_string();

        let mut signals = None;
        if let Some(listener) = listener {
            devtools.call("Runtime.enable", json!({}), Some(&session))?;
            devtools.call("Page.enable", json!({}), Some(&session))?;
            devtools.call("Runtime.addBinding", json!({ "name": listener.binding }), Some(&session))?;
            devtools.call("Page.addScriptToEvaluateOnNewDocument", json!({ "source": listener.script }), Some(&session))?;
            devtools.call("Runtime.evaluate", json!({ "expression": listener.script }), Some(&session))?;
            signals = Some(listener.signals);
        }

        let started_at = now();
        let params = json!({ "format": "png", "everyNthFrame": 1, "maxWidth": size[0] * scale, "maxHeight": size[1] * scale });
        devtools.call("Page.startScreencast", params, Some(&session))?;

        let stop = Arc::new(AtomicBool::new(false));
        let reader = {
            let (stop, frames) = (stop.clone(), frames.clone());
            thread::spawn(move || devtools.save_frames(&session, &frames, &stop, signals))
        };
        Ok(Screencast { stop, reader, frames, video: video.to_path_buf(), fps, started_at })
    }

    /// When the first frame of the video was asked for, in seconds of wall clock.
    pub fn started_at(&self) -> f64 {
        self.started_at
    }

    /// Stops recording and encodes what arrived into the video.
    pub fn finish(self) -> Result<()> {
        let stopped_at = now();
        self.stop.store(true, Ordering::Relaxed);
        let timestamps = self.reader.join().map_err(|_| anyhow!("the screencast reader panicked"))??;
        if timestamps.is_empty() {
            bail!("the browser sent no frames");
        }
        let mut encoder = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "image2pipe", "-framerate", &self.fps.to_string(), "-i", "-"])
            .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "10", "-pix_fmt", "yuv444p"])
            .arg(&self.video)
            .stdin(Stdio::piped())
            .spawn()
            .context("starting ffmpeg")?;
        let mut sink = encoder.stdin.take().expect("piped stdin");
        let mut loaded: Option<(usize, Vec<u8>)> = None;
        for frame in frame_per_tick(&timestamps, self.started_at, stopped_at, self.fps) {
            if loaded.as_ref().is_none_or(|(shown, _)| *shown != frame) {
                loaded = Some((frame, fs::read(self.frames.join(frame_name(frame)))?));
            }
            sink.write_all(&loaded.as_ref().expect("just loaded").1)?;
        }
        drop(sink);
        if !encoder.wait()?.success() {
            bail!("ffmpeg could not encode {}", self.video.display());
        }
        fs::remove_dir_all(&self.frames)?;
        Ok(())
    }
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("the clock is past 1970").as_secs_f64()
}

fn frame_name(index: usize) -> String {
    format!("{index:06}.png")
}

/// Which frame shows at each tick of a constant frame rate. The screencast sends a frame only when
/// the page changed, so a frame lasts until the next, and the first covers the wait before it.
/// Times come from the browser clock, the same one as `started_at` on this machine.
fn frame_per_tick(timestamps: &[f64], started_at: f64, stopped_at: f64, fps: u32) -> Vec<usize> {
    let ticks = ((stopped_at - started_at) * fps as f64).round() as usize;
    let mut frame = 0;
    (0..ticks)
        .map(|tick| {
            let at = started_at + tick as f64 / fps as f64;
            while timestamps.get(frame + 1).is_some_and(|next| *next <= at) {
                frame += 1;
            }
            frame
        })
        .collect()
}

fn page_target(targets: &Value, url: &str) -> Option<String> {
    targets["targetInfos"]
        .as_array()?
        .iter()
        .find(|t| t["type"] == "page" && t["url"] == url)
        .and_then(|t| t["targetId"].as_str())
        .map(String::from)
}

struct Devtools {
    socket: WebSocket<TcpStream>,
    next_id: u64,
}

impl Devtools {
    fn connect(url: &str) -> Result<Devtools> {
        let address = url.strip_prefix("ws://").and_then(|rest| rest.split('/').next()).with_context(|| format!("unexpected CDP address {url}"))?;
        let stream = TcpStream::connect(address).with_context(|| format!("connecting to {address}"))?;
        // A 2x PNG is base64 in a JSON message: well past tungstenite's default limits.
        let config = WebSocketConfig::default().max_message_size(None).max_frame_size(None);
        let (socket, _) = tungstenite::client::client_with_config(url, stream, Some(config)).map_err(|e| anyhow!("opening {url}: {e}"))?;
        Ok(Devtools { socket, next_id: 0 })
    }

    fn send(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<u64> {
        self.next_id += 1;
        let mut message = json!({ "id": self.next_id, "method": method, "params": params });
        if let Some(session) = session {
            message["sessionId"] = json!(session);
        }
        self.socket.send(Message::text(message.to_string()))?;
        Ok(self.next_id)
    }

    fn call(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        let id = self.send(method, params, session)?;
        loop {
            let message = self.socket.read()?;
            let Message::Text(text) = message else { continue };
            let reply: Value = serde_json::from_str(&text)?;
            if reply["id"] == id {
                if !reply["error"].is_null() {
                    bail!("{method}: {}", reply["error"]);
                }
                return Ok(reply["result"].clone());
            }
        }
    }

    /// Saves frames until told to stop, acknowledging each so Chrome paces itself to the disk and
    /// slows down rather than dropping quality. Returns each frame's capture time.
    fn save_frames(mut self, session: &str, dir: &Path, stop: &AtomicBool, signals: Option<mpsc::Sender<String>>) -> Result<Vec<f64>> {
        self.socket.get_ref().set_read_timeout(Some(Duration::from_millis(100)))?;
        let mut timestamps = Vec::new();
        let mut stop_id = None;
        loop {
            if stop_id.is_none() && stop.load(Ordering::Relaxed) {
                stop_id = Some(self.send("Page.stopScreencast", json!({}), Some(session))?);
            }
            let text = match self.socket.read() {
                Ok(Message::Text(text)) => text,
                Ok(_) => continue,
                Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
                Err(e) => return Err(e.into()),
            };
            let message: Value = serde_json::from_str(&text)?;
            if stop_id.is_some() && message["id"].as_u64() == stop_id {
                return Ok(timestamps);
            }
            if let (Some(signals), "Runtime.bindingCalled") = (&signals, message["method"].as_str().unwrap_or_default()) {
                let _ = signals.send(message["params"]["payload"].as_str().unwrap_or_default().to_string());
            }
            if message["method"] == "Page.screencastFrame" {
                let params = &message["params"];
                let png = decode_base64(params["data"].as_str().context("a frame without data")?)?;
                fs::write(dir.join(frame_name(timestamps.len())), png)?;
                timestamps.push(params["metadata"]["timestamp"].as_f64().context("a frame without a timestamp")?);
                self.send("Page.screencastFrameAck", json!({ "sessionId": params["sessionId"] }), Some(session))?;
            }
        }
    }
}

fn decode_base64(text: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut bits, mut pending) = (0u32, 0);
    for byte in text.bytes().take_while(|b| *b != b'=') {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => bail!("not base64: byte {byte}"),
        };
        bits = bits << 6 | value as u32;
        pending += 6;
        if pending >= 8 {
            pending -= 8;
            out.push((bits >> pending) as u8);
            bits &= (1 << pending) - 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_base64_with_and_without_padding() {
        assert_eq!(decode_base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode_base64("aGV5").unwrap(), b"hey");
        assert_eq!(decode_base64("+/+/").unwrap(), [0xfb, 0xff, 0xbf]);
        assert_eq!(decode_base64("").unwrap(), b"");
        assert!(decode_base64("a*b").is_err());
    }

    #[test]
    fn holds_each_frame_until_the_next_one_arrives() {
        assert_eq!(frame_per_tick(&[100.5, 101.0, 103.0], 100.0, 104.0, 2), [0, 0, 1, 1, 1, 1, 2, 2]);
    }

    #[test]
    fn keeps_the_timeline_when_the_page_never_changes() {
        assert_eq!(frame_per_tick(&[100.0], 100.0, 102.0, 2), [0, 0, 0, 0]);
    }

    #[test]
    fn skips_frames_that_arrive_between_ticks() {
        assert_eq!(frame_per_tick(&[100.0, 100.1, 100.2, 100.6], 100.0, 101.0, 2), [0, 2]);
    }

    #[test]
    fn picks_the_tab_showing_the_page() {
        let targets = json!({ "targetInfos": [
            { "type": "page", "url": "chrome://newtab/", "targetId": "blank" },
            { "type": "iframe", "url": "file:///demo.html", "targetId": "frame" },
            { "type": "page", "url": "file:///demo.html", "targetId": "demo" },
        ] });

        assert_eq!(page_target(&targets, "file:///demo.html").as_deref(), Some("demo"));
        assert_eq!(page_target(&targets, "file:///other.html"), None);
    }
}
