#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app_icon;
mod file_icon;
mod media;
mod pdf;
mod preview;
mod settings;
#[cfg(windows)]
mod startup;
#[cfg(windows)]
mod tray;
mod ui;
#[cfg(windows)]
mod watcher_instance;
#[cfg(windows)]
mod windows;

use std::ffi::OsString;
use std::path::PathBuf;

enum Command {
    Preview(PathBuf),
    Watch,
    Settings,
    About,
    Help,
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return if cfg!(windows) {
            Ok(Command::Watch)
        } else {
            Ok(Command::Help)
        };
    };
    let command = if first == "--help" || first == "-h" {
        Command::Help
    } else if first == "--watch" {
        Command::Watch
    } else if first == "--settings" {
        Command::Settings
    } else if first == "--about" {
        Command::About
    } else if first == "--preview" || first == "--" {
        Command::Preview(PathBuf::from(args.next().ok_or("Expected a file path.")?))
    } else if first.to_string_lossy().starts_with('-') {
        return Err(format!("Unknown option: {}", first.to_string_lossy()));
    } else {
        Command::Preview(PathBuf::from(first))
    };
    if args.next().is_some() {
        return Err("Expected one file path. Use quotes for paths containing spaces.".into());
    }
    Ok(command)
}

fn run() -> Result<(), String> {
    match parse_args(std::env::args_os().skip(1))? {
        Command::Help => {
            println!(
                "Hop - quick file previews\n\n\
                 Usage:\n  hop <path>\n  hop --preview <path>\n  hop -- <path>\n  hop --watch\n  hop --settings\n  hop --about\n\n\
                 Space / Escape: close preview\n\
                 Arrow keys: move Explorer selection in watcher previews\n\
                 --watch: run the Windows Explorer watcher and tray menu (Windows only).\n\
                 --settings: configure default volume and controls auto-hide delay.\n\
                 --about: show Hop's version and description.\n\
                 Linux: pass the selected file path from your file manager or terminal."
            );
            Ok(())
        }
        Command::Preview(path) => ui::show(path),
        Command::Settings => ui::show_settings(),
        Command::About => ui::show_about(),
        Command::Watch => {
            #[cfg(windows)]
            {
                windows::watch().map_err(|error| error.to_string())
            }
            #[cfg(not(windows))]
            {
                Err("--watch is only supported on Windows. Use hop <path> on Linux.".into())
            }
        }
    }
}

fn main() -> std::process::ExitCode {
    #[cfg(windows)]
    let console = match tray::attach_parent_console() {
        Ok(console) => console,
        Err(error) => {
            tray::show_error(&error);
            return std::process::ExitCode::FAILURE;
        }
    };
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Hop: {error}");
            #[cfg(windows)]
            if !console {
                tray::show_error(&error);
            }
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Command, String> {
        parse_args(args.iter().map(OsString::from))
    }

    #[test]
    fn accepts_paths_and_explicit_options() {
        for args in [
            vec!["some file.txt"],
            vec!["--preview", "some file.txt"],
            vec!["--", "some file.txt"],
        ] {
            assert!(
                matches!(parse(&args), Ok(Command::Preview(path)) if path == std::path::Path::new("some file.txt"))
            );
        }
        assert!(matches!(parse(&["--", "-file"]), Ok(Command::Preview(_))));
        assert!(matches!(parse(&["--watch"]), Ok(Command::Watch)));
        assert!(matches!(parse(&["--help"]), Ok(Command::Help)));
        assert!(matches!(parse(&["--settings"]), Ok(Command::Settings)));
        assert!(matches!(parse(&["--about"]), Ok(Command::About)));
    }

    #[test]
    fn rejects_missing_paths_unknown_options_and_extra_arguments() {
        for args in [
            vec!["--preview"],
            vec!["--"],
            vec!["--unknown"],
            vec!["one", "two"],
            vec!["--watch", "extra"],
            vec!["--settings", "extra"],
            vec!["--about", "extra"],
        ] {
            assert!(parse(&args).is_err());
        }
    }
}
