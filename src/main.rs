use std::path::{Path, PathBuf};

const USAGE: &str = "usage: reel <demo.json> [out.mp4] [--style key=value]...\n       reel --render <recording.json> [out.mp4] [--style key=value]...\n       reel edit <recording.json | video> [--no-open] [--browser]\n       reel rec open --session <name> <url> [--size WxH] [--state <file>]\n       reel rec start --session <name> [--state <file>] [--style key=value]...\n       reel rec type --session <name> <selector> <text>\n       reel rec hold --session <name> <ms>\n       reel rec cut --session <name> [--last <n>]\n       reel rec stop --session <name> [--no-render]";

fn main() {
    let (positional, style) = split_style_flags(std::env::args().skip(1).collect());
    if positional.first().is_some_and(|c| c == "rec") {
        if let Err(error) = rec(&positional[1..], &style) {
            eprintln!("reel rec: {error:#}");
            std::process::exit(1);
        }
        return;
    }
    if let [command, path, flags @ ..] = positional.as_slice() {
        if command == "edit" {
            if let Err(error) = reel::edit(Path::new(path), !flags.iter().any(|f| f == "--no-open"), flags.iter().any(|f| f == "--browser")) {
                eprintln!("reel: {error:#}");
                std::process::exit(1);
            }
            return;
        }
    }
    let result = match positional.as_slice() {
        [flag, recording, rest @ ..] if flag == "--render" => reel::render_file(Path::new(recording), rest.first().map(PathBuf::from), &style),
        [script, rest @ ..] if !script.starts_with('-') => reel::record(Path::new(script), rest.first().map(PathBuf::from), &style),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    match result {
        Ok(output) => println!("{}", output.display()),
        Err(error) => {
            eprintln!("reel: {error:#}");
            std::process::exit(1);
        }
    }
}

/// Takes `--name value` out of the arguments.
fn take_flag(args: &mut Vec<String>, name: &str) -> Option<String> {
    let at = args.iter().position(|a| a == name)?;
    args.remove(at);
    (at < args.len()).then(|| args.remove(at))
}

fn parse_size(text: &str) -> Result<[u32; 2], String> {
    let parsed = text.split_once('x').and_then(|(w, h)| Some([w.parse().ok()?, h.parse().ok()?]));
    parsed.filter(|[w, h]: &[u32; 2]| *w > 0 && *h > 0).ok_or(format!("{text} is not a size: use WxH"))
}

fn rec(args: &[String], style: &[String]) -> anyhow::Result<()> {
    use anyhow::{anyhow, bail, Context};
    use reel::recorder;

    let mut args = args.to_vec();
    let session = take_flag(&mut args, "--session").ok_or_else(|| anyhow!("which session? {USAGE}"))?;
    let verb = if args.is_empty() { String::new() } else { args.remove(0) };
    match verb.as_str() {
        "open" => {
            let size = take_flag(&mut args, "--size").map_or(Ok([1920, 1080]), |s| parse_size(&s)).map_err(|e| anyhow!(e))?;
            let state = take_flag(&mut args, "--state").map(PathBuf::from);
            let url = args.first().context("which address?")?;
            recorder::open(&session, url, size, state.as_deref())?;
        }
        "start" => {
            let state = take_flag(&mut args, "--state").map(PathBuf::from);
            recorder::start(&session, style, state.as_deref())?;
            println!("recording session {session}: run reel rec stop --session {session} when done");
        }
        "serve" => {
            let state = take_flag(&mut args, "--state").map(PathBuf::from);
            recorder::serve(&session, style, state)?;
        }
        "type" => {
            let [selector, text] = args.as_slice() else { bail!("expected a selector and the text to type") };
            recorder::type_text(&session, selector, text)?;
        }
        "hold" => {
            let ms = args.first().context("for how many milliseconds?")?.parse().context("reading the milliseconds")?;
            recorder::hold(&session, ms)?;
        }
        "cut" => {
            let count = take_flag(&mut args, "--last").map_or(Ok(1), |n| n.parse()).context("reading --last")?;
            println!("{}", recorder::cut(&session, count)?);
        }
        "stop" => {
            let done = recorder::stop(&session, !args.iter().any(|a| a == "--no-render"))?;
            println!("{}", serde_json::to_string_pretty(&done)?);
        }
        _ => bail!("{USAGE}"),
    }
    Ok(())
}

fn split_style_flags(args: Vec<String>) -> (Vec<String>, Vec<String>) {
    let (mut positional, mut style) = (Vec::new(), Vec::new());
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--style" {
            style.extend(args.next());
        } else {
            positional.push(arg);
        }
    }
    (positional, style)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_a_flag_and_its_value_out_of_the_arguments() {
        let mut args = ["--session", "demo", "open", "http://x"].map(String::from).to_vec();

        assert_eq!(take_flag(&mut args, "--session").as_deref(), Some("demo"));
        assert_eq!(args, ["open", "http://x"]);
        assert_eq!(take_flag(&mut args, "--size"), None);
        let mut dangling = vec!["--last".to_string()];
        assert_eq!(take_flag(&mut dangling, "--last"), None);
    }

    #[test]
    fn reads_a_size() {
        assert_eq!(parse_size("1280x720"), Ok([1280, 720]));
        assert!(parse_size("1280").is_err());
        assert!(parse_size("0x720").is_err());
        assert!(parse_size("axb").is_err());
    }

    #[test]
    fn takes_style_flags_from_anywhere_on_the_command_line() {
        let args = ["--style", "zoom.level=1.2", "demo.json", "out.mp4", "--style", "a.b=c"].map(String::from).to_vec();

        let (positional, style) = split_style_flags(args);

        assert_eq!(positional, ["demo.json", "out.mp4"]);
        assert_eq!(style, ["zoom.level=1.2", "a.b=c"]);
    }
}
