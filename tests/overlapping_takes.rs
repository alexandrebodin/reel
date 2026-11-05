use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

const FAKE_BROWSER: &str = r#"#!/bin/sh
echo "$2 $6" >> "$BROWSER_LOG"
if [ "$6" = open ]; then
  waited=0
  while [ ! -e "$RELEASE" ] && [ "$waited" -lt 250 ]; do sleep 0.02; waited=$((waited + 1)); done
  echo '{"success":false,"error":"no page in this test"}'
else
  echo '{"success":true,"data":{}}'
fi
"#;

struct Bench {
    root: PathBuf,
}

fn bench(name: &str) -> Bench {
    let root = std::env::temp_dir().join(format!("reel-overlap-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("tools")).unwrap();
    let browser = root.join("tools/agent-browser");
    fs::write(&browser, FAKE_BROWSER).unwrap();
    fs::set_permissions(&browser, fs::Permissions::from_mode(0o755)).unwrap();
    Bench { root }
}

impl Bench {
    fn script(&self, folder: &str) -> PathBuf {
        let dir = self.root.join(folder);
        fs::create_dir_all(&dir).unwrap();
        let script = dir.join("demo.json");
        fs::write(&script, r#"{ "url": "about:blank", "steps": [] }"#).unwrap();
        script
    }

    fn take(&self, script: &Path) -> Child {
        let path = format!("{}:{}", self.root.join("tools").display(), std::env::var("PATH").unwrap());
        Command::new(env!("CARGO_BIN_EXE_reel"))
            .arg(script)
            .env("PATH", path)
            .env("BROWSER_LOG", self.root.join("browser.log"))
            .env("RELEASE", self.root.join("release"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    fn log(&self) -> Vec<(String, String)> {
        let text = fs::read_to_string(self.root.join("browser.log")).unwrap_or_default();
        text.lines().map(|line| line.split_once(' ').unwrap()).map(|(session, command)| (session.to_string(), command.to_string())).collect()
    }

    fn sessions_that_ran(&self, command: &str) -> Vec<String> {
        self.log().into_iter().filter(|(_, c)| c == command).map(|(session, _)| session).collect()
    }

    fn wait_for_opens(&self, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.sessions_that_ran("open").len() < count {
            assert!(Instant::now() < deadline, "only saw {:?}", self.log());
            sleep(Duration::from_millis(10));
        }
    }

    fn release(&self) {
        fs::write(self.root.join("release"), "").unwrap();
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

#[test]
fn same_named_scripts_in_different_folders_never_share_a_browser() {
    let bench = bench("folders");
    let first = bench.take(&bench.script("one"));
    bench.wait_for_opens(1);
    let second = bench.take(&bench.script("two"));
    bench.wait_for_opens(2);
    bench.release();

    let (first, second) = (first.wait_with_output().unwrap(), second.wait_with_output().unwrap());

    assert!(stderr(&first).contains("no page in this test"), "{}", stderr(&first));
    assert!(stderr(&second).contains("no page in this test"), "{}", stderr(&second));
    let opened = bench.sessions_that_ran("open");
    assert_ne!(opened[0], opened[1]);
    let mut closed = bench.sessions_that_ran("close");
    closed.sort();
    let mut expected = opened;
    expected.sort();
    assert_eq!(closed, expected);
}

#[test]
fn a_second_take_of_the_same_script_stops_without_touching_the_first() {
    let bench = bench("same-script");
    let script = bench.script("one");
    let first = bench.take(&script);
    bench.wait_for_opens(1);

    let second = bench.take(&script).wait_with_output().unwrap();

    assert!(!second.status.success());
    assert!(stderr(&second).contains("already being recorded"), "{}", stderr(&second));
    assert_eq!(bench.log().len(), 1, "{:?}", bench.log());
    bench.release();
    assert!(stderr(&first.wait_with_output().unwrap()).contains("no page in this test"));
}

#[test]
fn the_same_script_can_record_again_once_the_first_take_ended() {
    let bench = bench("again");
    let script = bench.script("one");
    bench.release();

    let first = bench.take(&script).wait_with_output().unwrap();
    let second = bench.take(&script).wait_with_output().unwrap();

    assert!(stderr(&first).contains("no page in this test"), "{}", stderr(&first));
    assert!(stderr(&second).contains("no page in this test"), "{}", stderr(&second));
}
