//! duskpaper — switch the macOS wallpaper between a light and a dark image
//! whenever the system appearance changes.

mod core;
mod statusbar;
mod ui;

use std::{env, fs, process::ExitCode, thread, time::Duration};

use objc2::MainThreadMarker;
use objc2_foundation::NSRunLoop;

use crate::core::{Mode, config_path, current_mode, load_config};

const USAGE: &str = "\
duskpaper — light/dark wallpaper switcher

USAGE:
    duskpaper                                  open the app (menu bar + window)
    duskpaper app [--hidden]                   same; --hidden starts in the menu bar only
    duskpaper set <light-image> <dark-image>   save images for all displays and apply
    duskpaper apply                            apply once for the current mode
    duskpaper watch                            headless: follow changes in the foreground
    duskpaper install                          start the app at login (LaunchAgent)
    duskpaper uninstall                        remove the LaunchAgent
    duskpaper status                           show config and current mode";

fn main() -> ExitCode {
    let mtm = MainThreadMarker::new().expect("must run on main thread");
    let args: Vec<String> = env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let result = match args.as_slice() {
        [] | ["app"] => run_app(false),
        ["app", "--hidden"] => run_app(true),
        ["set", light, dark] => (|| {
            let mut cfg = load_config();
            cfg.set(None, Mode::Light, Some(core::absolute_image(light)?));
            cfg.set(None, Mode::Dark, Some(core::absolute_image(dark)?));
            core::save_config(&cfg)?;
            println!("saved {}", config_path().display());
            core::apply(mtm).map(|lines| lines.iter().for_each(|l| println!("{l}")))
        })(),
        ["apply"] => core::apply(mtm).map(|lines| lines.iter().for_each(|l| println!("{l}"))),
        ["watch"] => watch(mtm),
        ["install"] => core::install_agent(true).map(|log| {
            println!("installed {} (logs: {})", core::plist_path().display(), log.display())
        }),
        ["uninstall"] => core::uninstall_agent(true).map(|()| println!("uninstalled")),
        ["status"] => {
            status(mtm);
            Ok(())
        }
        ["help" | "--help" | "-h"] => {
            println!("{USAGE}");
            Ok(())
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("duskpaper: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_app(hidden: bool) -> Result<(), String> {
    // One instance only. Exit 0 so launchd's KeepAlive doesn't respawn us.
    let dir = core::cache_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let lock = fs::File::create(dir.join("app.lock")).map_err(|e| e.to_string())?;
    if lock.try_lock().is_err() {
        println!("duskpaper is already running (look for it in the menu bar)");
        return Ok(());
    }
    ui::run(hidden);
    drop(lock);
    Ok(())
}

fn watch(mtm: MainThreadMarker) -> Result<(), String> {
    core::apply_and_log(mtm);
    let _observers = core::observe_changes(move || {
        // The defaults write can land a moment after the notification.
        thread::sleep(Duration::from_millis(250));
        core::apply_and_log(mtm);
    });
    println!("duskpaper: watching for appearance changes");
    NSRunLoop::currentRunLoop().run();
    Ok(())
}

fn status(mtm: MainThreadMarker) {
    let cfg = load_config();
    println!("mode:    {}", current_mode().key());
    println!("config:  {}", config_path().display());
    for name in core::screen_names(mtm) {
        println!("{name}:");
        for mode in [Mode::Light, Mode::Dark] {
            let image = cfg.resolve(&name, mode);
            println!(
                "  {:<6} {}",
                mode.key(),
                image.map_or("(not set)".into(), |p| p.display().to_string())
            );
        }
    }
    println!("login:   {}", if core::agent_installed() { "installed" } else { "not installed" });
}
