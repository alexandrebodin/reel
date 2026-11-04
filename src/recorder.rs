use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, sleep};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::capture::{sign_in, AgentBrowser, Browser, Recording, OVERLAY_JS};
use crate::events::{Event, Pointer, Rect};
use crate::render::{plan_for, render, video_duration_ms};
use crate::screencast::{PageListener, Screencast};
use crate::script::Step;
use crate::style::Style;
use crate::timeline::Cut;

const BINDING: &str = "__reelSignal";
const FPS: u32 = 60;
const SAME_TYPING_GAP_MS: f64 = 2_500.0;
const CLICK_CAUSES_NAVIGATION_MS: f64 = 2_000.0;
const CUT_LEAD_MS: f64 = 250.0;
/// A live agent stops to think between actions. These keep what a viewer needs of each wait: a
/// moment to see the result, and room for the cursor and camera to reach the next target.
const SEEN_AFTER_ACTION_MS: f64 = 1_000.0;
const SEEN_BEFORE_ACTION_MS: f64 = 1_500.0;
const SEEN_AT_THE_START_MS: f64 = 500.0;
const SEEN_AT_THE_END_MS: f64 = 2_500.0;
const BETWEEN_KEYS: Duration = Duration::from_millis(45);
const OPEN_RETRIES: usize = 4;
const START_TIMEOUT: Duration = Duration::from_secs(30);

/// What the listener in the page reports, with `t` in milliseconds of wall clock.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Signal {
    Click { t: f64, target: Rect, frame: Rect, cursor: Option<String>, selector: String },
    /// `value` is left out for passwords and card numbers.
    Input { t: f64, target: Rect, frame: Rect, cursor: Option<String>, selector: String, value: Option<String> },
    Overlay { t: f64, frame: Rect },
    Navigation { t: f64, url: String },
    Hold { t: f64, until: f64 },
}

impl Signal {
    fn at(&self) -> f64 {
        match self {
            Signal::Click { t, .. } | Signal::Input { t, .. } | Signal::Overlay { t, .. } | Signal::Navigation { t, .. } | Signal::Hold { t, .. } => *t,
        }
    }

    fn is_action(&self) -> bool {
        matches!(self, Signal::Click { .. } | Signal::Input { .. } | Signal::Hold { .. })
    }
}

/// One take as it happens: everything the page reported and what the agent asked to cut. Times stay
/// in wall clock until the take is turned into events, so a signal's order never depends on when it arrived.
#[derive(Debug, Default)]
pub struct Take {
    started_ms: f64,
    signals: Vec<Signal>,
    cuts: Vec<(f64, f64)>,
}

impl Take {
    pub fn new(started_ms: f64) -> Take {
        Take { started_ms, ..Take::default() }
    }

    pub fn add(&mut self, signal: Signal) {
        let at = signal.at();
        let position = self.signals.iter().rposition(|s| s.at() <= at).map_or(0, |i| i + 1);
        self.signals.insert(position, signal);
    }

    /// Takes out the last `count` actions and everything since, and returns how many it found.
    /// A run of keystrokes in one field is one action.
    pub fn cut_last(&mut self, count: usize, now_ms: f64) -> usize {
        let starts = self.action_starts();
        if count == 0 || starts.is_empty() {
            return 0;
        }
        let found = count.min(starts.len());
        let from = starts[starts.len() - found];
        self.signals.retain(|s| s.at() < from);
        self.cuts.push((from - CUT_LEAD_MS, now_ms));
        found
    }

    fn action_starts(&self) -> Vec<f64> {
        let mut starts = Vec::new();
        let mut typing: Option<(&str, f64)> = None;
        for signal in &self.signals {
            if !signal.is_action() {
                continue;
            }
            if let Signal::Input { t, selector, .. } = signal {
                let same = typing.is_some_and(|(field, last)| field == selector && t - last <= SAME_TYPING_GAP_MS);
                typing = Some((selector, *t));
                if same {
                    continue;
                }
            } else {
                typing = None;
            }
            starts.push(signal.at());
        }
        starts
    }

    pub fn cuts(&self) -> Vec<Cut> {
        self.cuts.iter().map(|(from, to)| Cut { start_ms: (from - self.started_ms).max(0.0), end_ms: to - self.started_ms }).collect()
    }

    /// The agent's thinking time in a take of this length: the middle of every wait between actions.
    pub fn thinking(&self, duration_ms: f64) -> Vec<Cut> {
        let mut busy: Vec<(f64, f64)> = self.signals.iter().map(|s| (s.at(), if let Signal::Hold { until, .. } = s { *until } else { s.at() })).map(|(a, b)| (a - self.started_ms, b - self.started_ms)).collect();
        busy.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut cuts = Vec::new();
        let mut seen_until = SEEN_AT_THE_START_MS;
        for (from, to) in busy {
            let next_seen = from - SEEN_BEFORE_ACTION_MS;
            if next_seen > seen_until {
                cuts.push(Cut { start_ms: seen_until, end_ms: next_seen });
            }
            seen_until = seen_until.max(to + SEEN_AFTER_ACTION_MS);
        }
        let last = duration_ms - SEEN_AT_THE_END_MS + SEEN_AFTER_ACTION_MS;
        if !self.signals.is_empty() && last > seen_until {
            cuts.push(Cut { start_ms: seen_until, end_ms: last });
        }
        cuts
    }

    pub fn events(&self) -> Vec<Event> {
        let at = |t: f64| t - self.started_ms;
        let mut events: Vec<Event> = Vec::new();
        let mut typing: Option<(String, f64)> = None;
        for signal in &self.signals {
            match signal {
                Signal::Input { t, target, frame, cursor, selector, .. } => {
                    let extends = typing.as_ref().is_some_and(|(field, last)| field == selector && t - last <= SAME_TYPING_GAP_MS);
                    if extends {
                        if let Some(Event::Type { until_ms, .. }) = events.iter_mut().rev().find(|e| matches!(e, Event::Type { .. })) {
                            *until_ms = at(*t);
                        }
                    } else {
                        events.push(Event::Type { at_ms: at(*t), until_ms: at(*t), target: *target, frame: *frame, pointer: Pointer::from_css(cursor.as_deref()) });
                    }
                    typing = Some((selector.clone(), *t));
                }
                Signal::Click { t, target, frame, cursor, .. } => {
                    typing = None;
                    events.push(Event::Click { at_ms: at(*t), target: *target, frame: *frame, pointer: Pointer::from_css(cursor.as_deref()) });
                }
                Signal::Overlay { t, frame } => events.push(Event::Focus { at_ms: at(*t), frame: *frame }),
                Signal::Hold { t, until } => {
                    typing = None;
                    events.push(Event::Pause { at_ms: at(*t), until_ms: at(*until) });
                }
                Signal::Navigation { .. } => {}
            }
        }
        events
    }

    /// The take as a script to replay, and how many typed values it had to leave out.
    pub fn steps(&self) -> (Vec<Step>, usize) {
        let mut steps = Vec::new();
        let (mut left_out, mut last_click): (usize, Option<f64>) = (0, None);
        let mut typing: Option<(String, f64, bool)> = None;
        for signal in &self.signals {
            match signal {
                Signal::Click { t, selector, .. } => {
                    typing = None;
                    last_click = Some(*t);
                    steps.push(Step::Click { target: selector.clone() });
                }
                Signal::Input { t, selector, value, .. } => {
                    let extends = typing.as_ref().is_some_and(|(field, last, _)| field == selector && t - last <= SAME_TYPING_GAP_MS);
                    match (extends, value, typing.as_ref().is_some_and(|(.., saved)| *saved)) {
                        (true, Some(value), true) => {
                            if let Some(Step::Type { text, .. }) = steps.last_mut() {
                                text.clone_from(value);
                            }
                        }
                        (true, _, saved) => {
                            typing = Some((selector.clone(), *t, saved));
                            continue;
                        }
                        (false, Some(value), _) => steps.push(Step::Type { target: selector.clone(), text: value.clone() }),
                        (false, None, _) => left_out += 1,
                    }
                    typing = Some((selector.clone(), *t, value.is_some()));
                }
                Signal::Hold { t, until } => {
                    typing = None;
                    steps.push(Step::Pause { ms: (until - t).round() as u64 });
                }
                Signal::Navigation { t, url } => {
                    let from_a_click = last_click.is_some_and(|click| t - click <= CLICK_CAUSES_NAVIGATION_MS);
                    if !from_a_click {
                        steps.push(Step::Goto { url: url.clone() });
                    }
                }
                Signal::Overlay { .. } => {}
            }
        }
        (steps, left_out)
    }
}

/// Reports what the person using the page does, in the words a script uses, and is safe to install
/// twice. A selector here must find the same element when replayed: see `LOCATE_JS` for `text=`.
fn listener_js() -> String {
    format!(
        r#"(() => {{
  if (window.__reelListening) return;
  window.__reelListening = true;
  const send = (signal) => window.{BINDING}(JSON.stringify({{ t: Date.now(), ...signal }}));
  const box = (r) => ({{ x: r.x, y: r.y, w: r.width, h: r.height }});
  const clickable = 'button, a, summary, label, [role=button], [role=menuitem], [role=option], [role=radio], [role=tab], [role=checkbox]';
  const visible = (el) => el.getClientRects().length > 0;
  const label = (c) => (c.innerText || c.value || c.getAttribute('aria-label') || '').trim();
  const unique = (selector, el) => {{
    const all = document.querySelectorAll(selector);
    return all.length === 1 && all[0] === el;
  }};
  const quoted = (value) => `"${{value.replace(/["\\]/g, '\\$&')}}"`;
  const selectorOf = (el) => {{
    const generated = /^(base-ui-|radix-|headlessui-|react-aria|:r|_r_)/.test(el.id);
    if (el.id && !generated && unique(`[id=${{quoted(el.id)}}]`, el)) return /^[A-Za-z][\w-]*$/.test(el.id) ? `#${{el.id}}` : `[id=${{quoted(el.id)}}]`;
    for (const attribute of ['data-testid', 'data-test', 'name', 'aria-label', 'placeholder']) {{
      const value = el.getAttribute(attribute);
      const selector = value && `${{el.tagName.toLowerCase()}}[${{attribute}}=${{quoted(value)}}]`;
      if (value && unique(selector, el)) return selector;
    }}
    const text = label(el);
    if (text && text.length <= 60 && el.matches(clickable)) {{
      const top = [...document.querySelectorAll(clickable)].reverse().find((c) => visible(c) && label(c) === text);
      if (top === el) return `text=${{text}}`;
    }}
    const path = [];
    for (let a = el; a && a !== document.documentElement; a = a.parentElement) {{
      const same = a.parentElement ? [...a.parentElement.children] : [a];
      path.unshift(`${{a.tagName.toLowerCase()}}:nth-child(${{same.indexOf(a) + 1}})`);
    }}
    return path.join(' > ');
  }};
  const frameOf = (el) => {{
    const own = el.getBoundingClientRect();
    const layer = el.closest('dialog, [role=dialog], [role=alertdialog], [role=menu], [role=listbox]');
    if (layer) return layer.getBoundingClientRect();
    for (let a = el.parentElement; a && a !== document.body; a = a.parentElement) {{
      const r = a.getBoundingClientRect();
      if (r.width > innerWidth * 0.7 || r.height > innerHeight * 0.7) break;
      if (r.width >= Math.max(300, own.width * 2)) return r;
    }}
    return own;
  }};
  const describe = (el) => ({{ target: box(el.getBoundingClientRect()), frame: box(frameOf(el)), cursor: getComputedStyle(el).cursor, selector: selectorOf(el) }});
  addEventListener('click', (e) => {{
    if (!(e.target instanceof Element)) return;
    const el = e.target.closest(clickable) ?? e.target;
    send({{ kind: 'click', ...describe(el) }});
  }}, true);
  addEventListener('input', (e) => {{
    const el = e.target;
    if (!(el instanceof Element)) return;
    const secret = el.type === 'password' || /password|cc-|one-time-code/.test(el.getAttribute('autocomplete') || '');
    const value = !secret && 'value' in el ? el.value : undefined;
    send({{ kind: 'input', ...describe(el), value }});
  }}, true);
  send({{ kind: 'navigation', url: location.href }});
  const overlay = {OVERLAY_JS};
  let shown = null;
  let previous = null;
  setInterval(() => {{
    const now = overlay();
    const key = now && `${{now.x}},${{now.y}},${{now.w}},${{now.h}}`;
    const settled = key === previous;
    previous = key;
    if (settled && !!key !== !!shown) {{
      if (now) send({{ kind: 'overlay', frame: now }});
    }}
    if (settled) shown = key;
  }}, 200);
}})();"#
    )
}

fn now_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("the clock is past 1970").as_secs_f64() * 1_000.0
}

fn control_path(session: &str) -> PathBuf {
    std::env::temp_dir().join(format!("reel-rec-{session}.json"))
}

fn check_session(session: &str) -> Result<()> {
    if session.is_empty() || !session.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        bail!("a session name can only use letters, digits, - and _, not {session:?}");
    }
    Ok(())
}

/// Starts the browser at 2x for a session, ready to sign in or rehearse before recording.
pub fn open(session: &str, url: &str, size: [u32; 2], state: Option<&Path>) -> Result<()> {
    check_session(session)?;
    let mut browser = AgentBrowser::new(session, 2);
    if let Some(state) = state {
        sign_in(&mut browser, state, url)?;
    }
    open_page(&mut browser, url)?;
    browser.run(&["set", "viewport", &size[0].to_string(), &size[1].to_string(), "2"], None)?;
    let scale = browser.run(&["eval", "window.devicePixelRatio"], None)?;
    if scale["result"].as_f64() != Some(2.0) {
        bail!("session {session} already has a browser, which is not at 2x: run agent-browser --session {session} close, then this again");
    }
    Ok(())
}

/// A session closed a moment ago can drop the first command while its daemon shuts down.
fn open_page(browser: &mut dyn Browser, url: &str) -> Result<()> {
    for _ in 0..OPEN_RETRIES {
        match browser.run(&["open", url], None) {
            Err(error) if error.to_string().contains("no response came back") => sleep(Duration::from_millis(500)),
            other => return other.map(|_| ()),
        }
    }
    browser.run(&["open", url], None).map(|_| ())
}

/// Starts recording a session's browser in the background and returns once it is capturing.
pub fn start(session: &str, style_overrides: &[String], state: Option<&Path>) -> Result<()> {
    check_session(session)?;
    if request(session, "status").is_ok() {
        bail!("session {session} is already recording: run reel rec stop --session {session} first");
    }
    let _ = fs::remove_file(control_path(session));
    let dir = std::env::current_dir()?.join("out").join(session);
    fs::create_dir_all(&dir)?;
    let log = dir.join("rec.log");
    let output = fs::File::create(&log)?;
    let mut command = Command::new(std::env::current_exe()?);
    command.args(["rec", "serve", "--session", session]);
    for assignment in style_overrides {
        command.args(["--style", assignment]);
    }
    if let Some(state) = state {
        command.arg("--state").arg(state);
    }
    let mut child = command.stdin(Stdio::null()).stdout(output.try_clone()?).stderr(output).spawn().context("starting the recorder")?;
    let deadline = Instant::now() + START_TIMEOUT;
    while Instant::now() < deadline {
        if request(session, "status").is_ok() {
            return Ok(());
        }
        if child.try_wait()?.is_some() {
            let reason = fs::read_to_string(&log).unwrap_or_default();
            bail!("{}", reason.trim().trim_start_matches("reel rec: "));
        }
        sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    bail!("the recorder did not start in time: see {}", log.display())
}

/// Ends the take, writes its recording and replay script, and renders it unless told not to.
pub fn stop(session: &str, render_video: bool) -> Result<Value> {
    let mut reply = request(session, "stop")?;
    if render_video {
        let path = PathBuf::from(reply["recording"].as_str().context("the recorder gave no recording")?);
        let recording = Recording::load(&path)?;
        let video = path.with_file_name(format!("{session}.mp4"));
        render(&recording, &plan_for(&recording), &video)?;
        reply["video"] = json!(video);
    }
    Ok(reply)
}

pub fn cut(session: &str, count: usize) -> Result<Value> {
    request(session, &format!("cut {count}"))
}

/// Keeps the picture still for the viewer to read: an idle stretch would otherwise be sped up.
pub fn hold(session: &str, ms: u64) -> Result<Value> {
    let from = now_ms();
    sleep(Duration::from_millis(ms));
    request(session, &format!("hold {from} {}", now_ms()))
}

/// Types at a readable pace: `agent-browser type` puts the whole text in at once.
pub fn type_text(session: &str, selector: &str, text: &str) -> Result<()> {
    let mut browser = AgentBrowser::new(session, 2);
    browser.run(&["click", selector], None)?;
    for character in text.chars() {
        browser.run(&["type", selector, &character.to_string()], None)?;
        sleep(BETWEEN_KEYS);
    }
    Ok(())
}

fn request(session: &str, line: &str) -> Result<Value> {
    let not_recording = || anyhow!("session {session} is not recording: run reel rec start --session {session}");
    let control: Value = serde_json::from_slice(&fs::read(control_path(session)).map_err(|_| not_recording())?)?;
    let port = control["port"].as_u64().context("reading the recorder's port")? as u16;
    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).map_err(|_| not_recording())?;
    writeln!(stream, "{line}")?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    let reply: Value = serde_json::from_str(&reply).context("reading the recorder's answer")?;
    match reply["error"].as_str() {
        Some(error) => bail!("{error}"),
        None => Ok(reply),
    }
}

#[derive(Serialize)]
struct Control {
    port: u16,
    pid: u32,
}

/// The background half of `start`: captures until told to stop. It alone writes the take's state.
pub fn serve(session: &str, style_overrides: &[String], state: Option<PathBuf>) -> Result<()> {
    let style = live_defaults().overridden(style_overrides)?;
    let mut browser = AgentBrowser::new(session, 2);
    let page = browser.run(&["eval", "JSON.stringify({ w: innerWidth, h: innerHeight, scale: devicePixelRatio, url: location.href })"], None).with_context(|| format!("session {session} has no browser: run reel rec open --session {session} <url> first"))?;
    let page: Value = serde_json::from_str(page["result"].as_str().context("reading the page")?)?;
    if page["url"] == "about:blank" {
        bail!("session {session} has no page open: run reel rec open --session {session} <url> first");
    }
    let scale = page["scale"].as_f64().unwrap_or(1.0);
    if scale != 2.0 {
        bail!("session {session}'s browser is at {scale}x, which would record blurry: run agent-browser --session {session} close, then reel rec open --session {session} <url>");
    }
    let size = [page["w"].as_u64().context("reading the page width")? as u32, page["h"].as_u64().context("reading the page height")? as u32];
    let url = page["url"].as_str().context("reading the page address")?.to_string();
    let cdp = browser.run(&["get", "cdp-url"], None)?;
    let cdp = cdp["cdpUrl"].as_str().context("reading the CDP address")?.to_string();

    let dir = std::env::current_dir()?.join("out").join(session);
    fs::create_dir_all(&dir)?;
    let video = dir.join("raw.mp4");
    let (signals, incoming) = mpsc::channel::<String>();
    let listener = PageListener { binding: BINDING.to_string(), script: listener_js(), signals };
    let screencast = Screencast::start_with_listener(&cdp, &url, size, 2, &video, FPS, Some(listener))?;
    let take = Arc::new(Mutex::new(Take::new(screencast.started_at() * 1_000.0)));
    {
        let take = take.clone();
        thread::spawn(move || {
            for text in incoming {
                match serde_json::from_str::<Signal>(&text) {
                    Ok(signal) => take.lock().expect("take").add(signal),
                    Err(error) => eprintln!("reel: ignored a page signal ({error}): {text}"),
                }
            }
        });
    }

    let control = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = control.local_addr()?.port();
    fs::write(control_path(session), serde_json::to_vec(&Control { port, pid: std::process::id() })?)?;
    let mut screencast = Some(screencast);
    for stream in control.incoming() {
        let mut stream = stream?;
        let mut line = String::new();
        BufReader::new(stream.try_clone()?).read_line(&mut line)?;
        let (reply, finished) = match line.trim().split_whitespace().collect::<Vec<_>>().as_slice() {
            ["status"] => (json!({ "recording": true }), false),
            ["cut", count] => match count.parse() {
                Ok(count) => {
                    let found = take.lock().expect("take").cut_last(count, now_ms());
                    (json!({ "cut": found }), false)
                }
                Err(_) => (json!({ "error": format!("{count} is not a number of actions") }), false),
            },
            ["hold", from, to] => match (from.parse(), to.parse()) {
                (Ok(t), Ok(until)) => {
                    take.lock().expect("take").add(Signal::Hold { t, until });
                    (json!({}), false)
                }
                _ => (json!({ "error": "unreadable hold" }), false),
            },
            ["stop"] => {
                let finished = screencast.take().context("already stopped").and_then(|s| {
                    s.finish()?;
                    let take = take.lock().expect("take");
                    finish_take(&take, &dir, &video, &url, size, style.clone(), state.as_deref())
                });
                match finished {
                    Ok(reply) => (reply, true),
                    Err(error) => (json!({ "error": format!("{error:#}") }), true),
                }
            }
            _ => (json!({ "error": format!("unknown request {line:?}") }), false),
        };
        writeln!(stream, "{reply}")?;
        if finished {
            break;
        }
    }
    let _ = fs::remove_file(control_path(session));
    Ok(())
}

/// A live take is busier than a script, so it starts from a calmer camera.
fn live_defaults() -> Style {
    let mut style = Style::default();
    style.zoom.calm = true;
    style
}

fn finish_take(take: &Take, dir: &Path, video: &Path, url: &str, size: [u32; 2], style: Style, state: Option<&Path>) -> Result<Value> {
    let duration_ms = video_duration_ms(video)?;
    let recording = Recording { video: video.to_path_buf(), size, scale: 2, fps: FPS, duration_ms, events: take.events(), style, timeline: None, imported: false, cuts: [take.cuts(), take.thinking(duration_ms)].concat() };
    let recording_path = dir.join("recording.json");
    fs::write(&recording_path, serde_json::to_string_pretty(&recording)?)?;
    let (steps, left_out) = take.steps();
    let mut script = json!({ "url": url, "size": size, "style": recording.style, "steps": steps });
    if let Some(state) = state {
        script["state"] = json!(state.canonicalize().with_context(|| format!("reading {}", state.display()))?);
    }
    let replay = dir.join("replay.json");
    fs::write(&replay, serde_json::to_string_pretty(&script)?)?;
    Ok(json!({ "recording": recording_path, "replay": replay, "actions": steps.len(), "left_out_of_replay": left_out, "seconds": duration_ms / 1_000.0 }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: f64 = 1_000_000.0;
    const A: Rect = Rect { x: 10.0, y: 20.0, w: 100.0, h: 30.0 };
    const ROW: Rect = Rect { x: 0.0, y: 10.0, w: 700.0, h: 60.0 };

    fn click(at: f64, selector: &str) -> Signal {
        Signal::Click { t: START + at, target: A, frame: ROW, cursor: Some("pointer".into()), selector: selector.into() }
    }

    fn input(at: f64, selector: &str, value: Option<&str>) -> Signal {
        Signal::Input { t: START + at, target: A, frame: ROW, cursor: Some("text".into()), selector: selector.into(), value: value.map(String::from) }
    }

    fn navigation(at: f64, url: &str) -> Signal {
        Signal::Navigation { t: START + at, url: url.into() }
    }

    fn take(signals: Vec<Signal>) -> Take {
        let mut take = Take::new(START);
        for signal in signals {
            take.add(signal);
        }
        take
    }

    #[test]
    fn reads_what_the_page_sends() {
        let text = r##"{"t":5,"kind":"click","target":{"x":1,"y":2,"w":3,"h":4},"frame":{"x":0,"y":0,"w":9,"h":9},"cursor":"pointer","selector":"#add"}"##;

        let signal: Signal = serde_json::from_str(text).unwrap();

        assert!(matches!(signal, Signal::Click { t, ref selector, .. } if t == 5.0 && selector == "#add"));
        let secret: Signal = serde_json::from_str(r#"{"t":5,"kind":"input","target":{"x":1,"y":2,"w":3,"h":4},"frame":{"x":0,"y":0,"w":9,"h":9},"selector":"input"}"#).unwrap();
        assert!(matches!(secret, Signal::Input { value: None, .. }));
    }

    #[test]
    fn times_events_from_the_start_of_the_video() {
        let events = take(vec![click(2_000.0, "#a")]).events();

        assert_eq!(events, [Event::Click { at_ms: 2_000.0, target: A, frame: ROW, pointer: Pointer::Hand }]);
    }

    #[test]
    fn keeps_time_order_when_signals_arrive_out_of_order() {
        let events = take(vec![click(3_000.0, "#b"), click(1_000.0, "#a")]).events();

        assert_eq!(events[0].at_ms(), 1_000.0);
    }

    #[test]
    fn folds_keystrokes_in_one_field_into_one_typing_event() {
        let events = take(vec![input(1_000.0, "#h", Some("a")), input(1_100.0, "#h", Some("ab")), input(1_200.0, "#h", Some("abc"))]).events();

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Event::Type { at_ms, until_ms, pointer: Pointer::Text, .. } if at_ms == 1_000.0 && until_ms == 1_200.0));
    }

    #[test]
    fn starts_a_new_typing_event_for_another_field_a_long_gap_or_a_click_between() {
        let count = |signals| take(signals).events().len();

        assert_eq!(count(vec![input(0.0, "#a", None), input(100.0, "#b", None)]), 2);
        assert_eq!(count(vec![input(0.0, "#a", None), input(SAME_TYPING_GAP_MS, "#a", None)]), 1);
        assert_eq!(count(vec![input(0.0, "#a", None), input(SAME_TYPING_GAP_MS + 1.0, "#a", None)]), 2);
        assert_eq!(count(vec![input(0.0, "#a", None), click(50.0, "#x"), input(100.0, "#a", None)]), 3);
    }

    #[test]
    fn turns_an_overlay_into_something_to_look_at_and_a_hold_into_a_pause() {
        let events = take(vec![Signal::Overlay { t: START + 500.0, frame: ROW }, Signal::Hold { t: START + 900.0, until: START + 2_900.0 }]).events();

        assert_eq!(events, [Event::Focus { at_ms: 500.0, frame: ROW }, Event::Pause { at_ms: 900.0, until_ms: 2_900.0 }]);
    }

    #[test]
    fn writes_the_final_value_of_a_field_into_the_replay() {
        let (steps, left_out) = take(vec![click(0.0, "#add"), input(100.0, "#h", Some("a")), input(200.0, "#h", Some("ab")), click(900.0, "text=Add")]).steps();

        assert_eq!(steps, [Step::Click { target: "#add".into() }, Step::Type { target: "#h".into(), text: "ab".into() }, Step::Click { target: "text=Add".into() }]);
        assert_eq!(left_out, 0);
    }

    #[test]
    fn never_writes_a_secret_into_the_replay_and_counts_it_once_per_field() {
        let (steps, left_out) = take(vec![input(100.0, "#password", None), input(200.0, "#password", None), click(900.0, "#go")]).steps();

        assert_eq!(steps, [Step::Click { target: "#go".into() }]);
        assert_eq!(left_out, 1);
    }

    #[test]
    fn adds_a_goto_only_for_navigation_no_click_caused() {
        let (steps, _) = take(vec![click(1_000.0, "a"), navigation(1_000.0 + CLICK_CAUSES_NAVIGATION_MS, "http://x/next"), navigation(9_000.0, "http://x/other")]).steps();

        assert_eq!(steps, [Step::Click { target: "a".into() }, Step::Goto { url: "http://x/other".into() }]);
        let (late, _) = take(vec![click(1_000.0, "a"), navigation(1_001.0 + CLICK_CAUSES_NAVIGATION_MS, "http://x/next")]).steps();
        assert_eq!(late.len(), 2);
    }

    #[test]
    fn replays_a_hold_as_a_pause() {
        let (steps, _) = take(vec![Signal::Hold { t: START, until: START + 1_500.4 }]).steps();

        assert_eq!(steps, [Step::Pause { ms: 1_500 }]);
    }

    #[test]
    fn cuts_the_last_action_from_just_before_it_to_now() {
        let mut take = take(vec![click(1_000.0, "#a"), click(5_000.0, "#wrong"), Signal::Overlay { t: START + 5_400.0, frame: ROW }]);

        assert_eq!(take.cut_last(1, START + 8_000.0), 1);

        assert_eq!(take.events().len(), 1);
        assert_eq!(take.steps().0, [Step::Click { target: "#a".into() }]);
        assert_eq!(take.cuts(), [Cut { start_ms: 5_000.0 - CUT_LEAD_MS, end_ms: 8_000.0 }]);
    }

    #[test]
    fn counts_a_run_of_keystrokes_as_one_action_when_cutting() {
        let mut take = take(vec![click(0.0, "#a"), input(1_000.0, "#h", Some("a")), input(1_100.0, "#h", Some("ab"))]);

        assert_eq!(take.cut_last(1, START + 2_000.0), 1);

        assert_eq!(take.steps().0, [Step::Click { target: "#a".into() }]);
    }

    #[test]
    fn cuts_several_actions_and_no_more_than_there_are() {
        let mut several = take(vec![click(1_000.0, "#a"), click(2_000.0, "#b"), click(3_000.0, "#c")]);
        assert_eq!(several.cut_last(2, START + 4_000.0), 2);
        assert_eq!(several.events().len(), 1);

        let mut all = take(vec![click(1_000.0, "#a")]);
        assert_eq!(all.cut_last(5, START + 4_000.0), 1);
        assert_eq!(all.cuts(), [Cut { start_ms: 750.0, end_ms: 4_000.0 }]);
    }

    #[test]
    fn cuts_nothing_when_there_is_nothing_to_cut() {
        let mut empty = take(vec![navigation(10.0, "http://x")]);

        assert_eq!(empty.cut_last(1, START + 100.0), 0);
        assert_eq!(empty.cuts(), []);
        assert_eq!(take(vec![click(1.0, "#a")]).cut_last(0, START + 2.0), 0);
    }

    fn cut_spans(take: &Take, duration_ms: f64) -> Vec<(f64, f64)> {
        take.thinking(duration_ms).iter().map(|c| (c.start_ms, c.end_ms)).collect()
    }

    #[test]
    fn takes_out_the_middle_of_a_wait_and_keeps_both_ends() {
        let spans = cut_spans(&take(vec![click(1_000.0, "#a"), click(9_000.0, "#b")]), 11_000.0);

        assert_eq!(spans, [(1_000.0 + SEEN_AFTER_ACTION_MS, 9_000.0 - SEEN_BEFORE_ACTION_MS)]);
    }

    #[test]
    fn leaves_a_wait_short_enough_alone() {
        let exactly = 1_000.0 + SEEN_AFTER_ACTION_MS + SEEN_BEFORE_ACTION_MS;

        assert_eq!(cut_spans(&take(vec![click(1_000.0, "#a"), click(exactly, "#b")]), exactly + SEEN_AT_THE_END_MS), []);
        let spans = cut_spans(&take(vec![click(1_000.0, "#a"), click(exactly + 1.0, "#b")]), exactly + SEEN_AT_THE_END_MS);
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn trims_the_wait_before_the_first_action_and_after_the_last() {
        let spans = cut_spans(&take(vec![click(10_000.0, "#a")]), 30_000.0);

        assert_eq!(spans, [(SEEN_AT_THE_START_MS, 10_000.0 - SEEN_BEFORE_ACTION_MS), (10_000.0 + SEEN_AFTER_ACTION_MS, 30_000.0 - SEEN_AT_THE_END_MS + SEEN_AFTER_ACTION_MS)]);
    }

    #[test]
    fn never_trims_a_hold_the_agent_asked_for() {
        let hold = Signal::Hold { t: START + 3_000.0, until: START + 8_000.0 };
        let spans = cut_spans(&take(vec![click(2_000.0, "#a"), hold, click(9_000.0, "#b")]), 11_000.0);

        assert_eq!(spans, []);
    }

    #[test]
    fn counts_an_overlay_appearing_as_something_to_see() {
        let overlay = Signal::Overlay { t: START + 6_000.0, frame: ROW };

        let spans = cut_spans(&take(vec![click(1_000.0, "#a"), overlay, click(20_000.0, "#b")]), 22_000.0);

        assert_eq!(spans, [(2_000.0, 4_500.0), (7_000.0, 18_500.0)]);
    }

    #[test]
    fn leaves_a_take_with_no_actions_alone() {
        assert_eq!(cut_spans(&Take::new(START), 60_000.0), []);
    }

    #[test]
    fn does_not_cut_before_the_start_of_the_video() {
        let mut early = take(vec![click(10.0, "#a")]);
        early.cut_last(1, START + 100.0);

        assert_eq!(early.cuts()[0].start_ms, 0.0);
    }

    struct ClosingBrowser {
        failures: usize,
        tries: usize,
        error: &'static str,
    }

    impl Browser for ClosingBrowser {
        fn run(&mut self, _args: &[&str], _stdin: Option<&str>) -> Result<Value> {
            self.tries += 1;
            if self.tries <= self.failures {
                bail!("{}", self.error);
            }
            Ok(json!({}))
        }

        fn start_recording(&mut self, _video: &Path, _size: [u32; 2], _scale: u32, _fps: u32) -> Result<()> {
            Ok(())
        }

        fn stop_recording(&mut self) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn retries_opening_a_page_while_an_old_session_shuts_down_but_not_on_other_errors() {
        let dropped = "the command reached the daemon but no response came back";

        let mut closing = ClosingBrowser { failures: 2, tries: 0, error: dropped };
        assert!(open_page(&mut closing, "http://x").is_ok());
        assert_eq!(closing.tries, 3);

        let mut gone = ClosingBrowser { failures: 99, tries: 0, error: dropped };
        assert!(open_page(&mut gone, "http://x").is_err());
        assert_eq!(gone.tries, OPEN_RETRIES + 1);

        let mut bad = ClosingBrowser { failures: 99, tries: 0, error: "net::ERR_NAME_NOT_RESOLVED" };
        assert!(open_page(&mut bad, "http://x").is_err());
        assert_eq!(bad.tries, 1);
    }

    #[test]
    fn starts_a_live_take_with_a_calm_camera_that_can_be_turned_off() {
        assert!(live_defaults().zoom.calm);
        assert!(!live_defaults().overridden(&["zoom.calm=false".to_string()]).unwrap().zoom.calm);
    }

    #[test]
    fn refuses_a_session_name_that_could_leave_the_folder() {
        assert!(check_session("demo-1_a").is_ok());
        assert!(check_session("../evil").is_err());
        assert!(check_session("").is_err());
    }

    #[test]
    fn says_a_session_is_not_recording() {
        let error = request("no-such-session-here", "status").unwrap_err();

        assert!(error.to_string().contains("not recording"), "{error}");
    }

    #[test]
    fn keeps_secrets_out_of_the_page_listener() {
        let script = listener_js();

        assert!(script.contains("password|cc-|one-time-code"));
        assert!(script.contains(BINDING));
    }

    #[test]
    fn does_not_trust_generated_ids_in_the_replay() {
        let script = listener_js();

        for generated in ["base-ui-", "radix-", ":r", "_r_"] {
            assert!(script.contains(generated), "{generated}");
        }
    }
}
