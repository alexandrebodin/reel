use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::events::{Event, Rect};
use crate::script::{Script, Step};

/// Runs one agent-browser command and returns its `data`.
pub trait Browser {
    fn run(&mut self, args: &[&str], stdin: Option<&str>) -> Result<Value>;
}

pub struct AgentBrowser {
    session: String,
}

impl AgentBrowser {
    pub fn new(session: &str) -> AgentBrowser {
        AgentBrowser { session: session.to_string() }
    }
}

impl Browser for AgentBrowser {
    fn run(&mut self, args: &[&str], stdin: Option<&str>) -> Result<Value> {
        let mut child = Command::new("agent-browser")
            .args(["--session", &self.session, "--json"])
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
}

/// Gaps that keep the take watchable: the cursor needs time to travel, typing needs a rhythm.
pub struct Pacing {
    pub between_actions: Duration,
    pub between_keys: Duration,
    pub settle: Duration,
}

impl Default for Pacing {
    fn default() -> Pacing {
        Pacing {
            between_actions: Duration::from_millis(900),
            between_keys: Duration::from_millis(45),
            settle: Duration::from_millis(1_000),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Recording {
    pub video: PathBuf,
    pub size: [u32; 2],
    pub fps: u32,
    pub duration_ms: f64,
    pub events: Vec<Event>,
}

pub fn capture(script: &Script, browser: &mut dyn Browser, video: &Path, pacing: &Pacing) -> Result<Recording> {
    let video_arg = video.to_string_lossy().to_string();
    let fps = script.fps.to_string();
    match &script.state {
        Some(state) => navigate(browser, &["--state", &state.to_string_lossy(), "open", &script.url])?,
        None => navigate(browser, &["open", &script.url])?,
    }
    browser.run(&["set", "viewport", &script.size[0].to_string(), &script.size[1].to_string()], None)?;
    let mut last_action = Instant::now() - pacing.between_actions;
    for step in &script.setup {
        perform(browser, step, pacing, &mut last_action, &|| 0.0)?;
    }
    sleep(pacing.settle);

    browser.run(&["record", "start", &video_arg, "--fps", &fps], None)?;
    let start = Instant::now();
    let elapsed = || start.elapsed().as_secs_f64() * 1_000.0;
    sleep(pacing.settle);

    let mut events = Vec::new();
    for step in &script.steps {
        events.extend(perform(browser, step, pacing, &mut last_action, &elapsed)?);
    }
    sleep(pacing.settle);
    let duration_ms = elapsed();
    browser.run(&["record", "stop"], None)?;

    Ok(Recording { video: video.to_path_buf(), size: script.size, fps: script.fps, duration_ms, events })
}

/// Runs one step and returns what it did on screen, timed by `elapsed`.
fn perform(
    browser: &mut dyn Browser,
    step: &Step,
    pacing: &Pacing,
    last_action: &mut Instant,
    elapsed: &dyn Fn() -> f64,
) -> Result<Option<Event>> {
    match step {
        Step::Click { target } => {
            let (rect, selector) = locate(browser, target)?;
            sleep(pacing.between_actions.saturating_sub(last_action.elapsed()));
            let event = Event::Click { at_ms: elapsed(), target: rect };
            browser.run(&["click", &selector], None)?;
            *last_action = Instant::now();
            Ok(Some(event))
        }
        Step::Type { target, text } => {
            let (rect, selector) = locate(browser, target)?;
            sleep(pacing.between_actions.saturating_sub(last_action.elapsed()));
            let at_ms = elapsed();
            browser.run(&["click", &selector], None)?;
            for character in text.chars() {
                browser.run(&["type", &selector, &character.to_string()], None)?;
                sleep(pacing.between_keys);
            }
            *last_action = Instant::now();
            Ok(Some(Event::Type { at_ms, until_ms: elapsed(), target: rect }))
        }
        Step::Pause { ms } => {
            let at_ms = elapsed();
            sleep(Duration::from_millis(*ms));
            Ok(Some(Event::Pause { at_ms, until_ms: elapsed() }))
        }
        Step::WaitFor { target } => {
            locate(browser, target)?;
            Ok(None)
        }
        Step::Goto { url } => {
            navigate(browser, &["open", url])?;
            Ok(None)
        }
    }
}

/// A dev server can keep a page loading past agent-browser's timeout; the next step that looks
/// for an element decides whether the page is usable.
fn navigate(browser: &mut dyn Browser, args: &[&str]) -> Result<()> {
    match browser.run(args, None) {
        Err(error) if error.to_string().contains("timed out") => Ok(()),
        other => other.map(|_| ()),
    }
}

/// Finds a visible element, scrolls it into view, tags it so the click hits that same element,
/// and returns where it sits in the viewport.
fn locate(browser: &mut dyn Browser, target: &str) -> Result<(Rect, String)> {
    let script = format!("({LOCATE_JS})({})", serde_json::to_string(target)?);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let found = &browser.run(&["eval", "--stdin"], Some(&script))?["result"];
        if let (Some(id), Some(x), Some(y), Some(w), Some(h)) =
            (found["id"].as_str(), found["x"].as_f64(), found["y"].as_f64(), found["w"].as_f64(), found["h"].as_f64())
        {
            return Ok((Rect { x, y, w, h }, format!("[data-reel=\"{id}\"]")));
        }
        if Instant::now() > deadline {
            bail!("no visible element for {target}");
        }
        sleep(Duration::from_millis(150));
    }
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
  const r = el.getBoundingClientRect();
  return { id, x: r.x, y: r.y, w: r.width, h: r.height };
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
    }

    impl Browser for FakeBrowser {
        fn run(&mut self, args: &[&str], _stdin: Option<&str>) -> Result<Value> {
            self.commands.push(args.join(" "));
            if self.slow_pages && args.contains(&"open") {
                bail!("Operation timed out. The page may still be loading or the element may not exist.");
            }
            if args[0] == "eval" {
                return Ok(json!({ "result": { "id": "abc", "x": 10.0, "y": 20.0, "w": 100.0, "h": 30.0 } }));
            }
            Ok(json!({}))
        }
    }

    fn script(steps: Vec<Step>) -> Script {
        Script { url: "about:blank".into(), size: [1920, 1080], fps: 60, state: None, setup: vec![], steps }
    }

    fn no_pacing() -> Pacing {
        Pacing { between_actions: Duration::ZERO, between_keys: Duration::ZERO, settle: Duration::ZERO }
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

        let target = Rect { x: 10.0, y: 20.0, w: 100.0, h: 30.0 };
        assert!(matches!(recording.events[0], Event::Click { target: t, .. } if t == target));
        assert!(matches!(recording.events[1], Event::Type { target: t, .. } if t == target));
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
    fn signs_in_with_the_state_file_when_opening() {
        let mut browser = FakeBrowser::default();
        let mut demo = script(vec![]);
        demo.state = Some(PathBuf::from("/auth/premium.json"));

        capture(&demo, &mut browser, Path::new("/tmp/raw.webm"), &no_pacing()).unwrap();

        assert_eq!(browser.commands[0], "--state /auth/premium.json open about:blank");
    }

    #[test]
    fn shifts_events_by_the_recorder_start_lag() {
        let target = Rect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 };
        let mut recording = Recording {
            video: PathBuf::from("raw.webm"),
            size: [1920, 1080],
            fps: 60,
            duration_ms: 10_000.0,
            events: vec![Event::Click { at_ms: 2_000.0, target }],
        };

        align_to_video(&mut recording, 10_300.0);

        assert_eq!(recording.events, vec![Event::Click { at_ms: 2_300.0, target }]);
        assert_eq!(recording.duration_ms, 10_300.0);
    }
}
