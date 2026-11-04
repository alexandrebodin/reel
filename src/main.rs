use std::path::{Path, PathBuf};

const USAGE: &str = "usage: reel <demo.json> [out.mp4] [--style key=value]...\n       reel --render <recording.json> [out.mp4] [--style key=value]...\n       reel edit <recording.json> [--no-open]";

fn main() {
    let (positional, style) = split_style_flags(std::env::args().skip(1).collect());
    if let [command, path, flags @ ..] = positional.as_slice() {
        if command == "edit" {
            if let Err(error) = reel::edit(Path::new(path), !flags.iter().any(|f| f == "--no-open")) {
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
    fn takes_style_flags_from_anywhere_on_the_command_line() {
        let args = ["--style", "zoom.level=1.2", "demo.json", "out.mp4", "--style", "a.b=c"].map(String::from).to_vec();

        let (positional, style) = split_style_flags(args);

        assert_eq!(positional, ["demo.json", "out.mp4"]);
        assert_eq!(style, ["zoom.level=1.2", "a.b=c"]);
    }
}
