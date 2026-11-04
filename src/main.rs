use std::path::{Path, PathBuf};

const USAGE: &str = "usage: reel <demo.json> [out.mp4]\n       reel --render <recording.json> [out.mp4]";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [flag, recording, rest @ ..] if flag == "--render" => reel::render_file(Path::new(recording), rest.first().map(PathBuf::from)),
        [script, rest @ ..] if !script.starts_with('-') => reel::record(Path::new(script), rest.first().map(PathBuf::from)),
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
