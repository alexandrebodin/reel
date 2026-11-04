use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::events::{Event, Pointer, Rect};
use crate::plan::Size;
use crate::screencast::Screencast;
use crate::script::{Script, Step};
use crate::style::Style;
use crate::timeline::{Cut, Timeline};

/// Runs one agent-browser command and returns its `data`.
pub trait Browser {
    fn run(&mut self, args: &[&str], stdin: Option<&str>) -> Result<Value>;
    fn start_recording(&mut self, video: &Path, size: [u32; 2], scale: u32, fps: u32) -> Result<()>;
    fn stop_recording(&mut self) -> Result<()>;
}

pub struct AgentBrowser {
    session: String,
    launch_args: String,
    screencast: Option<Screencast>,
}

impl AgentBrowser {
    pub fn new(session: &str, scale: u32) -> AgentBrowser {
        // Emulating a device scale is not enough: Chrome's screencast then still sends 1x frames.
        AgentBrowser { session: session.to_string(), launch_args: format!("--force-device-scale-factor={scale}"), screencast: None }
    }
}

impl Browser for AgentBrowser {
    fn run(&mut self, args: &[&str], stdin: Option<&str>) -> Result<Value> {
        let mut child = Command::new("agent-browser")
            .args(["--session", &self.session, "--json", "--args", &self.launch_args])
            .args(args)
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("starting agent-browser")?;
        if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(input.as_bytes())?;
        }
        let output = child.wait_with_output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout.lines().rev().find(|l| l.trim_start().starts_with('{')).unwrap_or_default();
        let response: Value = serde_json::from_str(line)
            .with_context(|| format!("agent-browser {}: {}", args.join(" "), String::from_utf8_lossy(&output.stderr)))?;
        if response["success"] != Value::Bool(true) {
            bail!("agent-browser {}: {}", args.join(" "), response["error"]);
        }
        Ok(response["data"].clone())
    }

    fn start_recording(&mut self, video: &Path, size: [u32; 2], scale: u32, fps: u32) -> Result<()> {
        let cdp_url = self.run(&["get", "cdp-url"], None)?;
        let cdp_url = cdp_url["cdpUrl"].as_str().context("reading the CDP address")?;
        let page_url = self.run(&["get", "url"], None)?;
        let page_url = page_url["url"].as_str().context("reading the page address")?;
        self.screencast = Some(Screencast::start(cdp_url, page_url, size, scale, video, fps)?);
        Ok(())
    }

    fn stop_recording(&mut self) -> Result<()> {
        self.screencast.take().context("not recording")?.finish()
    }
}

/// Gaps that keep the take watchable: the cursor needs time to travel, typing needs a rhythm.
pub struct Pacing {
    pub between_actions: Duration,
    pub between_keys: Duration,
    pub settle: Duration,
    /// Time for what a click opens to finish appearing before the camera looks for it.
    pub after_click: Duration,
}

impl Default for Pacing {
    fn default() -> Pacing {
        Pacing {
            between_actions: Duration::from_millis(1_300),
            between_keys: Duration::from_millis(45),
            settle: Duration::from_millis(1_000),
            after_click: Duration::from_millis(350),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Recording {
    pub video: PathBuf,
    pub size: [u32; 2],
    /// Device pixels per CSS pixel in the video. Events stay in CSS pixels.
    #[serde(default = "one")]
    pub scale: u32,
    pub fps: u32,
    pub duration_ms: f64,
    pub events: Vec<Event>,
    /// The look to render with: `--render` reproduces the take without its script.
    #[serde(default)]
    pub style: Style,
    /// Set once someone edits the take. Until then a render runs the automatic pass, so `style`
    /// keeps deciding things like how far the camera zooms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline: Option<Timeline>,
    /// Came from an existing video, with no event log: the automatic pass reads the pixels instead.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub imported: bool,
    /// Mistakes the agent took out while recording. They stay out when the timeline is reset.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cuts: Vec<Cut>,
}

impl Recording {
    pub fn load(path: &Path) -> Result<Recording> {
        let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut recording: Recording = serde_json::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
        recording.timeline = recording.timeline.map(|t| t.validated(recording.duration_ms)).transpose().with_context(|| format!("reading {}", path.display()))?;
        Ok(recording)
    }

    pub fn frame(&self) -> Size {
        Size { w: self.size[0] as f64, h: self.size[1] as f64 }
    }

    /// The saved edits, or what the automatic pass makes of the take.
    pub fn resolved_timeline(&self) -> Timeline {
        self.timeline.clone().unwrap_or_else(|| Timeline::auto_around_cuts(&self.events, self.duration_ms, self.frame(), &self.style, &self.cuts))
    }
}

fn one() -> u32 {
    1
}

pub fn capture(script: &Script, browser: &mut dyn Browser, video: &Path, pacing: &Pacing) -> Result<Recording> {
    if let Some(state) = &script.state {
        sign_in(browser, state, &script.url)?;
    }
    navigate(browser, &["open", &script.url])?;
    browser.run(&["set", "viewport", &script.size[0].to_string(), &script.size[1].to_string(), &script.scale.to_string()], None)?;
    let mut last_action = Instant::now() - pacing.between_actions;
    for step in &script.setup {
        perform(browser, step, pacing, &mut last_action, &|| 0.0)?;
    }
    sleep(pacing.settle);

    browser.start_recording(video, script.size, script.scale, script.fps)?;
    let start = Instant::now();
    let elapsed = || start.elapsed().as_secs_f64() * 1_000.0;
    sleep(pacing.settle);

    let mut events = Vec::new();
    for step in &script.steps {
        events.extend(perform(browser, step, pacing, &mut last_action, &elapsed)?);
    }
    sleep(pacing.settle);
    let duration_ms = elapsed();
    browser.stop_recording()?;

    Ok(Recording { video: video.to_path_buf(), size: script.size, scale: script.scale, fps: script.fps, duration_ms, events, style: script.style.clone(), timeline: None, imported: false, cuts: vec![] })
}

/// Runs one step and returns what it showed on screen, timed by `elapsed`.
fn perform(
    browser: &mut dyn Browser,
    step: &Step,
    pacing: &Pacing,
    last_action: &mut Instant,
    elapsed: &dyn Fn() -> f64,
) -> Result<Vec<Event>> {
    match step {
        Step::Click { target } => {
            let found = locate(browser, target)?;
            sleep(pacing.between_actions.saturating_sub(last_action.elapsed()));
            let mut events = vec![Event::Click { at_ms: elapsed(), target: found.target, frame: found.frame, pointer: found.pointer }];
            browser.run(&["click", &found.selector], None)?;
            *last_action = Instant::now();
            sleep(pacing.after_click);
            if let Some(frame) = open_overlay(browser)? {
                events.push(Event::Focus { at_ms: elapsed(), frame });
            }
            Ok(events)
        }
        Step::Type { target, text } => {
            let found = locate(browser, target)?;
            sleep(pacing.between_actions.saturating_sub(last_action.elapsed()));
            let at_ms = elapsed();
            browser.run(&["click", &found.selector], None)?;
            for character in text.chars() {
                browser.run(&["type", &found.selector, &character.to_string()], None)?;
                sleep(pacing.between_keys);
            }
            *last_action = Instant::now();
            Ok(vec![Event::Type { at_ms, until_ms: elapsed(), target: found.target, frame: found.frame, pointer: found.pointer }])
        }
        Step::Pause { ms } => {
            let at_ms = elapsed();
            sleep(Duration::from_millis(*ms));
            *last_action = Instant::now();
            Ok(vec![Event::Pause { at_ms, until_ms: elapsed() }])
        }
        Step::WaitFor { target } => {
            let found = locate(browser, target)?;
            *last_action = Instant::now();
            Ok(vec![Event::Focus { at_ms: elapsed(), frame: found.frame }])
        }
        Step::Goto { url } => {
            navigate(browser, &["open", url])?;
            Ok(vec![])
        }
    }
}

#[derive(Deserialize)]
struct BrowserState {
    cookies: Vec<StateCookie>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StateCookie {
    name: String,
    value: String,
    path: Option<String>,
    expires: Option<f64>,
    #[serde(default)]
    http_only: bool,
    same_site: Option<String>,
}

/// Sets the state file's live cookies itself: agent-browser's --state dropped Studio's session
/// cookie. Scoping them to the demo's origin also lets a state saved on another port work.
pub fn sign_in(browser: &mut dyn Browser, state: &Path, url: &str) -> Result<()> {
    let text = std::fs::read_to_string(state).with_context(|| format!("reading {}", state.display()))?;
    let state: BrowserState = serde_json::from_str(&text).context("parsing the state file")?;
    let origin = origin_of(url);
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64();
    browser.run(&["open"], None)?;
    for cookie in &state.cookies {
        let expires = cookie.expires.filter(|e| *e >= 0.0);
        if expires.is_some_and(|e| e <= now) {
            continue;
        }
        let mut args = vec!["cookies", "set", &cookie.name, &cookie.value, "--url", origin];
        if let Some(path) = &cookie.path {
            args.extend(["--path", path]);
        }
        if cookie.http_only {
            args.push("--httpOnly");
        }
        if let Some(same_site) = &cookie.same_site {
            args.extend(["--sameSite", same_site]);
        }
        let expires = expires.map(|e| (e as i64).to_string());
        if let Some(expires) = &expires {
            args.extend(["--expires", expires]);
        }
        browser.run(&args, None)?;
    }
    Ok(())
}

fn origin_of(url: &str) -> &str {
    let host_start = url.find("://").map_or(0, |i| i + 3);
    url[host_start..].find('/').map_or(url, |i| &url[..host_start + i])
}

/// A dev server can keep a page loading past agent-browser's timeout; the next step that looks
/// for an element decides whether the page is usable.
fn navigate(browser: &mut dyn Browser, args: &[&str]) -> Result<()> {
    match browser.run(args, None) {
        Err(error) if error.to_string().contains("timed out") => Ok(()),
        other => other.map(|_| ()),
    }
}

struct Located {
    target: Rect,
    frame: Rect,
    selector: String,
    pointer: Pointer,
}

fn rect_from(value: &Value) -> Option<Rect> {
    Some(Rect { x: value["x"].as_f64()?, y: value["y"].as_f64()?, w: value["w"].as_f64()?, h: value["h"].as_f64()? })
}

/// Finds a visible element, scrolls it into view, tags it so the click hits that same element,
/// and returns where it sits in the viewport and the container worth keeping in view around it.
fn locate(browser: &mut dyn Browser, target: &str) -> Result<Located> {
    let script = format!("({LOCATE_JS})({})", serde_json::to_string(target)?);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let found = &browser.run(&["eval", "--stdin"], Some(&script))?["result"];
        if let (Some(id), Some(rect), Some(frame)) = (found["id"].as_str(), rect_from(found), rect_from(&found["frame"])) {
            return Ok(Located { target: rect, frame, selector: format!("[data-reel=\"{id}\"]"), pointer: Pointer::from_css(found["cursor"].as_str()) });
        }
        if Instant::now() > deadline {
            bail!("no visible element for {target}");
        }
        sleep(Duration::from_millis(150));
    }
}

fn open_overlay(browser: &mut dyn Browser) -> Result<Option<Rect>> {
    let found = browser.run(&["eval", "--stdin"], Some(&format!("({OVERLAY_JS})()")))?;
    Ok(rect_from(&found["result"]))
}

const LOCATE_JS: &str = r#"(target) => {
  const visible = (el) => el.getClientRects().length > 0;
  let el = null;
  if (target.startsWith('text=')) {
    const want = target.slice(5).trim();
    const label = (c) => (c.innerText || c.value || c.getAttribute('aria-label') || '').trim();
    const clickable = 'button, a, summary, label, [role=button], [role=menuitem], [role=option], [role=radio], [role=tab], [role=checkbox]';
    // The last match is the topmost layer: menus and dialogs render at the end of the page.
    el = [...document.querySelectorAll(clickable)].reverse().find((c) => visible(c) && label(c) === want) ?? null;
  } else {
    el = [...document.querySelectorAll(target)].find(visible) ?? null;
  }
  if (!el) return null;
  el.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  const id = Math.random().toString(36).slice(2, 10);
  el.setAttribute('data-reel', id);
  const box = (r) => ({ x: r.x, y: r.y, w: r.width, h: r.height });
  const own = el.getBoundingClientRect();
  const frameOf = () => {
    const layer = el.closest('dialog, [role=dialog], [role=alertdialog], [role=menu], [role=listbox]');
    if (layer) return layer.getBoundingClientRect();
    for (let a = el.parentElement; a && a !== document.body; a = a.parentElement) {
      const r = a.getBoundingClientRect();
      if (r.width > innerWidth * 0.7 || r.height > innerHeight * 0.7) break;
      if (r.width >= Math.max(300, own.width * 2)) return r;
    }
    return own;
  };
  return { id, ...box(own), frame: box(frameOf()), cursor: getComputedStyle(el).cursor };
}"#;

pub const OVERLAY_JS: &str = r#"() => {
  const layers = [...document.querySelectorAll('dialog[open], [role=dialog], [role=alertdialog], [role=menu], [role=listbox]')]
    .filter((el) => el.getClientRects().length > 0);
  const top = layers[layers.length - 1];
  if (!top) return null;
  const r = top.getBoundingClientRect();
  return r.width < 40 || r.height < 40 ? null : { x: r.x, y: r.y, w: r.width, h: r.height };
}"#;

/// The recorder starts before its command returns, so events trail the video by that delay.
pub fn align_to_video(recording: &mut Recording, video_duration_ms: f64) {
    let lag_ms = (video_duration_ms - recording.duration_ms).clamp(0.0, 1_500.0);
    recording.events = recording.events.iter().map(|e| e.shifted(lag_ms)).collect();
    recording.duration_ms = video_duration_ms;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::Script;
    use serde_json::json;

    #[derive(Default)]
    struct FakeBrowser {
        commands: Vec<String>,
        slow_pages: bool,
        overlay: Option<Value>,
    }

    const TARGET: Rect = Rect { x: 10.0, y: 20.0, w: 100.0, h: 30.0 };
    const ROW: Rect = Rect { x: 0.0, y: 10.0, w: 700.0, h: 60.0 };

    impl Browser for FakeBrowser {
        fn run(&mut self, args: &[&str], stdin: Option<&str>) -> Result<Value> {
            self.commands.push(args.join(" "));
            if self.slow_pages && args.contains(&"open") {
                bail!("Operation timed out. The page may still be loading or the element may not exist.");
            }
            if args[0] == "eval" {
                if !stdin.unwrap_or_default().contains("data-reel") {
                    return Ok(json!({ "result": self.overlay.clone() }));
                }
                return Ok(json!({ "result": {
                    "id": "abc",
                    "x": TARGET.x, "y": TARGET.y, "w": TARGET.w, "h": TARGET.h,
                    "frame": { "x": ROW.x, "y": ROW.y, "w": ROW.w, "h": ROW.h },
                    "cursor": "pointer",
                } }));
            }
            Ok(json!({}))
        }

        fn start_recording(&mut self, _video: &Path, _size: [u32; 2], _scale: u32, _fps: u32) -> Result<()> {
            self.commands.push("record start".into());
            Ok(())
        }

        fn stop_recording(&mut self) -> Result<()> {
            self.commands.push("record stop".into());
            Ok(())
        }
    }

    fn script(steps: Vec<Step>) -> Script {
        Script { url: "about:blank".into(), size: [1920, 1080], fps: 60, scale: 2, style: Style::default(), state: None, setup: vec![], steps }
    }

    fn no_pacing() -> Pacing {
        Pacing { between_actions: Duration::ZERO, between_keys: Duration::ZERO, settle: Duration::ZERO, after_click: Duration::ZERO }
    }

    #[test]
    fn records_each_action_on_the_element_it_located() {
        let mut browser = FakeBrowser::default();
        let steps = vec![
            Step::Click { target: "text=Add domain".into() },
            Step::Type { target: "input".into(), text: "ab".into() },
            Step::Pause { ms: 5 },
        ];

        let recording = capture(&script(steps), &mut browser, Path::new("/tmp/raw.webm"), &no_pacing()).unwrap();

        assert!(matches!(recording.events[0], Event::Click { target, frame, pointer: Pointer::Hand, .. } if target == TARGET && frame == ROW));
        assert!(matches!(recording.events[1], Event::Type { target, frame, .. } if target == TARGET && frame == ROW));
        assert!(matches!(recording.events[2], Event::Pause { .. }));
        let clicks: Vec<&String> = browser.commands.iter().filter(|c| c.starts_with("click")).collect();
        assert_eq!(clicks, ["click [data-reel=\"abc\"]", "click [data-reel=\"abc\"]"]);
        assert!(browser.commands.contains(&"type [data-reel=\"abc\"] b".to_string()));
        let start = browser.commands.iter().position(|c| c.starts_with("record start")).unwrap();
        let stop = browser.commands.iter().position(|c| c == "record stop").unwrap();
        let click = browser.commands.iter().position(|c| c.starts_with("click")).unwrap();
        assert!(start < click && click < stop);
    }

    #[test]
    fn captures_at_the_scripts_device_scale() {
        let mut browser = FakeBrowser::default();

        let recording = capture(&script(vec![]), &mut browser, Path::new("/tmp/raw.mp4"), &no_pacing()).unwrap();

        assert!(browser.commands.contains(&"set viewport 1920 1080 2".to_string()));
        assert_eq!(recording.scale, 2);
    }

    #[test]
    fn keeps_the_scripts_style_with_the_take() {
        let mut demo = script(vec![]);
        demo.style.zoom.level = 1.1;

        let recording = capture(&demo, &mut FakeBrowser::default(), Path::new("/tmp/raw.mp4"), &no_pacing()).unwrap();

        assert_eq!(recording.style.zoom.level, 1.1);
    }

    #[test]
    fn reads_a_recording_from_before_device_scales_as_1x() {
        let recording: Recording =
            serde_json::from_str(r#"{ "video": "raw.webm", "size": [1920, 1080], "fps": 60, "duration_ms": 1.0, "events": [] }"#).unwrap();

        assert_eq!(recording.scale, 1);
    }

    #[test]
    fn leaves_the_cursor_time_to_travel_after_a_pause_or_a_wait() {
        for step in [Step::Pause { ms: 1 }, Step::WaitFor { target: "#done".into() }] {
            let mut browser = FakeBrowser::default();
            let pacing = Pacing { between_actions: Duration::from_millis(60), ..no_pacing() };
            let steps = vec![step, Step::Click { target: "#go".into() }];

            let recording = capture(&script(steps), &mut browser, Path::new("/tmp/raw.webm"), &pacing).unwrap();

            let gap = recording.events[1].at_ms() - recording.events[0].until_ms();
            assert!(gap >= 50.0, "the click came {gap}ms after {:?}", recording.events[0]);
        }
    }

    #[test]
    fn focuses_on_a_dialog_that_a_click_opened() {
        let mut browser = FakeBrowser {
            overlay: Some(json!({ "x": 700.0, "y": 300.0, "w": 500.0, "h": 400.0 })),
            ..FakeBrowser::default()
        };
        let steps = vec![Step::Click { target: "#open".into() }];

        let recording = capture(&script(steps), &mut browser, Path::new("/tmp/raw.webm"), &no_pacing()).unwrap();

        let dialog = Rect { x: 700.0, y: 300.0, w: 500.0, h: 400.0 };
        assert!(matches!(recording.events[0], Event::Click { .. }));
        assert!(matches!(recording.events[1], Event::Focus { frame, .. } if frame == dialog));
        assert_eq!(recording.events.len(), 2);
    }

    #[test]
    fn logs_no_focus_when_a_click_opened_nothing() {
        let mut browser = FakeBrowser::default();
        let steps = vec![Step::Click { target: "#go".into() }];

        let recording = capture(&script(steps), &mut browser, Path::new("/tmp/raw.webm"), &no_pacing()).unwrap();

        assert_eq!(recording.events.len(), 1);
    }

    #[test]
    fn focuses_on_what_the_script_waited_for() {
        let mut browser = FakeBrowser::default();
        let steps = vec![Step::WaitFor { target: "#result".into() }];

        let recording = capture(&script(steps), &mut browser, Path::new("/tmp/raw.webm"), &no_pacing()).unwrap();

        assert!(matches!(recording.events[0], Event::Focus { frame, .. } if frame == ROW));
    }

    #[test]
    fn runs_setup_steps_before_recording_and_logs_none_of_them() {
        let mut browser = FakeBrowser::default();
        let mut demo = script(vec![Step::Click { target: "#go".into() }]);
        demo.setup = vec![Step::Goto { url: "about:setup".into() }, Step::Click { target: "#cookie-ok".into() }];

        let recording = capture(&demo, &mut browser, Path::new("/tmp/raw.webm"), &no_pacing()).unwrap();

        let start = browser.commands.iter().position(|c| c.starts_with("record start")).unwrap();
        let setup = browser.commands.iter().position(|c| c == "open about:setup").unwrap();
        assert!(setup < start);
        assert_eq!(recording.events.len(), 1);
    }

    #[test]
    fn keeps_going_when_a_page_is_still_loading_after_the_navigation_timeout() {
        let mut browser = FakeBrowser { slow_pages: true, ..FakeBrowser::default() };
        let mut demo = script(vec![Step::Goto { url: "about:next".into() }, Step::Click { target: "#go".into() }]);
        demo.setup = vec![Step::Goto { url: "about:setup".into() }];

        let recording = capture(&demo, &mut browser, Path::new("/tmp/raw.webm"), &no_pacing()).unwrap();

        assert_eq!(recording.events.len(), 1);
    }

    #[test]
    fn sets_the_state_files_live_cookies_for_the_demo_origin_before_opening_it() {
        let dir = std::env::temp_dir().join(format!("reel-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let state = dir.join("state.json");
        std::fs::write(
            &state,
            r#"{ "cookies": [
                { "name": "token", "value": "a%2Bb", "domain": "localhost", "path": "/", "expires": 4102444800.5, "httpOnly": true, "sameSite": "Lax" },
                { "name": "stale", "value": "old", "domain": "localhost", "path": "/", "expires": 1000.0, "httpOnly": true, "sameSite": "Lax" },
                { "name": "for_the_session", "value": "x", "domain": "localhost", "path": "/", "expires": -1, "httpOnly": false, "sameSite": "None" }
            ] }"#,
        )
        .unwrap();
        let mut browser = FakeBrowser::default();
        let mut demo = script(vec![]);
        demo.url = "http://localhost:4000/app/".into();
        demo.state = Some(state);

        capture(&demo, &mut browser, Path::new("/tmp/raw.webm"), &no_pacing()).unwrap();

        let cookies: Vec<&String> = browser.commands.iter().filter(|c| c.starts_with("cookies set")).collect();
        assert_eq!(
            cookies,
            [
                "cookies set token a%2Bb --url http://localhost:4000 --path / --httpOnly --sameSite Lax --expires 4102444800",
                "cookies set for_the_session x --url http://localhost:4000 --path / --sameSite None",
            ]
        );
        let set = browser.commands.iter().position(|c| c.starts_with("cookies set")).unwrap();
        let opened = browser.commands.iter().position(|c| c == "open http://localhost:4000/app/").unwrap();
        assert!(set < opened);
    }

    #[test]
    fn shifts_events_by_the_recorder_start_lag() {
        let target = Rect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 };
        let mut recording = Recording {
            video: PathBuf::from("raw.webm"),
            size: [1920, 1080],
            scale: 1,
            fps: 60,
            duration_ms: 10_000.0,
            events: vec![Event::Click { at_ms: 2_000.0, target, frame: target, pointer: Pointer::Arrow }],
            style: Style::default(),
            timeline: None,
            imported: false,
            cuts: vec![],
        };

        align_to_video(&mut recording, 10_300.0);

        assert_eq!(recording.events, vec![Event::Click { at_ms: 2_300.0, target, frame: target, pointer: Pointer::Arrow }]);
        assert_eq!(recording.duration_ms, 10_300.0);
    }
}
