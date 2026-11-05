use std::fs;
use std::io::Write;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const TOOLS: [&str; 13] = ["sh", "curl", "tar", "gzip", "mkdir", "mv", "rm", "ln", "grep", "mktemp", "readlink", "cat", "cp"];

struct Machine {
    home: PathBuf,
    tools: PathBuf,
    releases: PathBuf,
}

fn on_path(tool: &str) -> PathBuf {
    let path = std::env::var_os("PATH").unwrap();
    std::env::split_paths(&path).map(|dir| dir.join(tool)).find(|candidate| candidate.exists()).unwrap_or_else(|| panic!("{tool} is not installed"))
}

fn executable(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn machine(name: &str, os: &str, arch: &str) -> Machine {
    let root = std::env::temp_dir().join(format!("reel-install-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let (home, tools, releases) = (root.join("home"), root.join("tools"), root.join("releases"));
    for dir in [&home, &tools, &releases] {
        fs::create_dir_all(dir).unwrap();
    }
    for tool in TOOLS {
        symlink(on_path(tool), tools.join(tool)).unwrap();
    }
    executable(&tools.join("uname"), &format!(r#"[ "$1" = -s ] && echo {os} || echo {arch}"#));
    Machine { home, tools, releases }
}

impl Machine {
    fn publish(&self, target: &str, version: &str) {
        let staging = self.releases.join(format!("staging-{target}"));
        fs::create_dir_all(staging.join("skills/reel")).unwrap();
        executable(&staging.join("reel"), &format!("echo reel {version}"));
        fs::write(staging.join("skills/reel/SKILL.md"), version).unwrap();
        let archive = self.releases.join(format!("reel-{target}.tar.gz"));
        let made = Command::new("tar").arg("-czf").arg(&archive).arg("-C").arg(&staging).args(["reel", "skills"]).status().unwrap();
        assert!(made.success());
        fs::remove_dir_all(&staging).unwrap();
    }

    fn install_with_path(&self, path: &str) -> Output {
        let script = fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/install.sh")).unwrap();
        let mut child = Command::new(self.tools.join("sh"))
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", path)
            .env("SHELL", "/bin/zsh")
            .env("REEL_RELEASES", format!("file://{}", self.releases.display()))
            .env("RELEASES_DIR", &self.releases)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&script).unwrap();
        child.wait_with_output().unwrap()
    }

    fn install(&self) -> Output {
        self.install_with_path(self.tools.to_str().unwrap())
    }

    fn installed(&self) -> PathBuf {
        self.home.join(".reel/bin/reel")
    }

    fn run_installed(&self) -> String {
        let output = Command::new(self.installed()).output().unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

#[test]
fn installs_the_build_for_each_platform() {
    for (os, arch, target) in [("Darwin", "arm64", "aarch64-apple-darwin"), ("Darwin", "x86_64", "x86_64-apple-darwin"), ("Linux", "x86_64", "x86_64-unknown-linux-musl")] {
        let machine = machine(&format!("{os}-{arch}"), os, arch);
        machine.publish(target, "1.0.0");

        let output = machine.install();

        assert!(output.status.success(), "{target}: {}", stderr(&output));
        assert_eq!(machine.run_installed(), "reel 1.0.0");
        assert_eq!(fs::read_to_string(machine.home.join(".reel/skills/reel/SKILL.md")).unwrap(), "1.0.0");
    }
}

#[test]
fn refuses_a_platform_with_no_build_and_installs_nothing() {
    let machine = machine("unsupported", "Linux", "aarch64");

    let output = machine.install();

    assert!(!output.status.success());
    assert!(stderr(&output).contains("Linux aarch64"), "{}", stderr(&output));
    assert!(!machine.installed().exists());
}

#[test]
fn a_failed_download_says_so_and_installs_nothing() {
    let machine = machine("no-release", "Darwin", "arm64");

    let output = machine.install();

    assert!(!output.status.success());
    assert!(stderr(&output).contains("reel-aarch64-apple-darwin.tar.gz"), "{}", stderr(&output));
    assert!(!machine.installed().exists());
}

#[test]
fn falls_back_to_gh_for_a_private_repo_and_keeps_reading_the_piped_script() {
    let machine = machine("private", "Darwin", "arm64");
    machine.publish("aarch64-apple-darwin", "1.0.0");
    let hidden = machine.releases.join("hidden");
    fs::create_dir_all(&hidden).unwrap();
    fs::rename(machine.releases.join("reel-aarch64-apple-darwin.tar.gz"), hidden.join("reel-aarch64-apple-darwin.tar.gz")).unwrap();
    executable(&machine.tools.join("gh"), r#"cat > /dev/null; echo "$4" > "$HOME/gh-repo"; cp "$RELEASES_DIR/hidden/$6" "$8""#);

    let output = machine.install();

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(machine.run_installed(), "reel 1.0.0");
    assert_eq!(fs::read_to_string(machine.home.join("gh-repo")).unwrap().trim(), "alexandrebodin/reel");
}

#[test]
fn reinstalling_replaces_the_build_and_skill_and_adds_to_the_path_once() {
    let machine = machine("again", "Darwin", "arm64");
    machine.publish("aarch64-apple-darwin", "1.0.0");
    assert!(machine.install().status.success());
    let leftover = machine.home.join(".reel/skills/reel/old.md");
    fs::write(&leftover, "from an earlier version").unwrap();
    machine.publish("aarch64-apple-darwin", "2.0.0");

    let output = machine.install();

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(machine.run_installed(), "reel 2.0.0");
    assert!(!leftover.exists());
    let rc = fs::read_to_string(machine.home.join(".zshrc")).unwrap();
    assert_eq!(rc.matches(".reel/bin").count(), 1, "{rc}");
}

#[test]
fn leaves_the_shell_profile_alone_when_already_on_the_path() {
    let machine = machine("on-path", "Darwin", "arm64");
    machine.publish("aarch64-apple-darwin", "1.0.0");

    let output = machine.install_with_path(&format!("{}:{}", machine.home.join(".reel/bin").display(), machine.tools.display()));

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!machine.home.join(".zshrc").exists());
}

#[test]
fn links_the_skill_only_for_agents_on_this_machine() {
    let machine = machine("skill", "Darwin", "arm64");
    machine.publish("aarch64-apple-darwin", "1.0.0");
    fs::create_dir_all(machine.home.join(".claude")).unwrap();

    let output = machine.install();

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(fs::read_to_string(machine.home.join(".claude/skills/reel/SKILL.md")).unwrap(), "1.0.0");
    assert!(!machine.home.join(".agents").exists());
}

#[test]
fn says_where_the_skill_is_when_no_agent_is_found() {
    let machine = machine("no-agent", "Darwin", "arm64");
    machine.publish("aarch64-apple-darwin", "1.0.0");

    let output = machine.install();

    assert!(stdout(&output).contains(".reel/skills/reel"), "{}", stdout(&output));
}

#[test]
fn keeps_a_skill_link_the_person_made_and_says_so() {
    let machine = machine("own-link", "Darwin", "arm64");
    machine.publish("aarch64-apple-darwin", "1.0.0");
    let elsewhere = machine.home.join("checkout/skills/reel");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::create_dir_all(machine.home.join(".agents/skills")).unwrap();
    symlink(&elsewhere, machine.home.join(".agents/skills/reel")).unwrap();

    let output = machine.install();

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(fs::read_link(machine.home.join(".agents/skills/reel")).unwrap(), elsewhere);
    assert!(stdout(&output).contains(".agents/skills/reel"), "{}", stdout(&output));
}

#[test]
fn reinstalling_keeps_its_own_skill_link_quietly() {
    let machine = machine("own-skill", "Darwin", "arm64");
    machine.publish("aarch64-apple-darwin", "1.0.0");
    fs::create_dir_all(machine.home.join(".agents")).unwrap();
    assert!(machine.install().status.success());

    let output = machine.install();

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!stdout(&output).contains("Left"), "{}", stdout(&output));
    assert_eq!(fs::read_to_string(machine.home.join(".agents/skills/reel/SKILL.md")).unwrap(), "1.0.0");
}

#[test]
fn installs_anyway_but_names_the_tools_reel_needs() {
    let machine = machine("missing-tools", "Darwin", "arm64");
    machine.publish("aarch64-apple-darwin", "1.0.0");

    let output = machine.install();

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("ffmpeg"), "{}", stdout(&output));
    assert!(stdout(&output).contains("agent-browser"), "{}", stdout(&output));
}

#[test]
fn stays_quiet_about_tools_that_are_there() {
    let machine = machine("has-tools", "Darwin", "arm64");
    machine.publish("aarch64-apple-darwin", "1.0.0");
    executable(&machine.tools.join("ffmpeg"), "true");
    executable(&machine.tools.join("agent-browser"), "true");

    let output = machine.install();

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!stdout(&output).contains("ffmpeg"), "{}", stdout(&output));
    assert!(!stdout(&output).contains("agent-browser"), "{}", stdout(&output));
}
