//! duskpaper — switch the macOS wallpaper between a light and a dark image
//! whenever the system appearance changes.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    ptr::NonNull,
    thread,
    time::Duration,
};

use block2::RcBlock;
use objc2::{MainThreadMarker, runtime::NSObjectProtocol};
use objc2_app_kit::{
    NSScreen, NSWorkspace, NSWorkspaceActiveSpaceDidChangeNotification,
    NSWorkspaceDidWakeNotification, NSWorkspaceScreensDidWakeNotification,
};
use objc2_foundation::{
    NSDictionary, NSDistributedNotificationCenter, NSNotification, NSRunLoop, NSString, NSURL,
    NSUserDefaults,
};

const LABEL: &str = "dev.duskpaper.agent";

// ---------------------------------------------------------------- config

struct Config {
    light: PathBuf,
    dark: PathBuf,
}

fn home() -> PathBuf {
    PathBuf::from(env::var("HOME").expect("HOME is not set"))
}

fn config_path() -> PathBuf {
    home().join(".config/duskpaper/config")
}

fn load_config() -> Result<Config, String> {
    let path = config_path();
    let text = fs::read_to_string(&path).map_err(|_| {
        format!(
            "no config at {} — run `duskpaper set <light-image> <dark-image>` first",
            path.display()
        )
    })?;
    let (mut light, mut dark) = (None, None);
    for line in text.lines() {
        match line.split_once('=') {
            Some(("light", v)) => light = Some(PathBuf::from(v.trim())),
            Some(("dark", v)) => dark = Some(PathBuf::from(v.trim())),
            _ => {}
        }
    }
    match (light, dark) {
        (Some(light), Some(dark)) => Ok(Config { light, dark }),
        _ => Err(format!("config at {} is missing light= or dark=", path.display())),
    }
}

fn save_config(cfg: &Config) -> Result<(), String> {
    let path = config_path();
    fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let text = format!("light={}\ndark={}\n", cfg.light.display(), cfg.dark.display());
    fs::write(&path, text).map_err(|e| e.to_string())
}

fn absolute_image(arg: &str) -> Result<PathBuf, String> {
    let p = Path::new(arg);
    let p = p
        .canonicalize()
        .map_err(|e| format!("{arg}: {e}"))?;
    if !p.is_file() {
        return Err(format!("{} is not a file", p.display()));
    }
    Ok(p)
}

// ---------------------------------------------------------------- macOS

fn is_dark() -> bool {
    let defaults = NSUserDefaults::standardUserDefaults();
    defaults
        .stringForKey(&NSString::from_str("AppleInterfaceStyle"))
        .is_some_and(|s| s.to_string().eq_ignore_ascii_case("dark"))
}

/// Set `image` on every screen (current Space) unless it is already set.
fn set_wallpaper(mtm: MainThreadMarker, image: &Path) -> Result<bool, String> {
    let workspace = NSWorkspace::sharedWorkspace();
    let url = NSURL::fileURLWithPath(&NSString::from_str(&image.to_string_lossy()));
    let options = NSDictionary::new();
    let mut changed = false;
    for screen in NSScreen::screens(mtm).iter() {
        let current = workspace.desktopImageURLForScreen(&screen);
        if current.is_some_and(|c| c.isEqual(Some(&url))) {
            continue;
        }
        // SAFETY: empty options dictionary, valid file URL and screen.
        unsafe { workspace.setDesktopImageURL_forScreen_options_error(&url, &screen, &options) }
            .map_err(|e| e.localizedDescription().to_string())?;
        changed = true;
    }
    Ok(changed)
}

fn apply(mtm: MainThreadMarker) -> Result<(), String> {
    let cfg = load_config()?;
    let dark = is_dark();
    let image = if dark { &cfg.dark } else { &cfg.light };
    if set_wallpaper(mtm, image)? {
        println!("{} mode → {}", if dark { "dark" } else { "light" }, image.display());
    }
    Ok(())
}

fn watch(mtm: MainThreadMarker) -> ! {
    if let Err(e) = apply(mtm) {
        eprintln!("duskpaper: {e}");
    }

    let handler = RcBlock::new(move |_n: NonNull<NSNotification>| {
        // The defaults write can land a hair after the notification.
        thread::sleep(Duration::from_millis(250));
        if let Err(e) = apply(mtm) {
            eprintln!("duskpaper: {e}");
        }
    });

    // Light/dark changed (manual toggle or Auto at sunrise/sunset).
    let dist = NSDistributedNotificationCenter::defaultCenter();
    let _theme = unsafe {
        dist.addObserverForName_object_queue_usingBlock(
            Some(&NSString::from_str("AppleInterfaceThemeChangedNotification")),
            None,
            None,
            &handler,
        )
    };

    // setDesktopImageURL only affects the current Space, so re-check when
    // switching Spaces and after waking.
    let ws_center = NSWorkspace::sharedWorkspace().notificationCenter();
    let _observers: Vec<_> = unsafe {
        [
            NSWorkspaceActiveSpaceDidChangeNotification,
            NSWorkspaceDidWakeNotification,
            NSWorkspaceScreensDidWakeNotification,
        ]
        .into_iter()
        .map(|name| {
            ws_center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &handler)
        })
        .collect()
    };

    println!("duskpaper: watching for appearance changes");
    NSRunLoop::currentRunLoop().run();
    unreachable!("run loop exited")
}

// ---------------------------------------------------------------- launch agent

fn plist_path() -> PathBuf {
    home().join(format!("Library/LaunchAgents/{LABEL}.plist"))
}

fn install() -> Result<(), String> {
    load_config()?; // fail early if not configured
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let log = home().join("Library/Logs/duskpaper.log");
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array><string>{}</string><string>watch</string></array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>ProcessType</key><string>Interactive</string>
    <key>StandardOutPath</key><string>{}</string>
    <key>StandardErrorPath</key><string>{}</string>
</dict>
</plist>
"#,
        exe.display(),
        log.display(),
        log.display()
    );
    let path = plist_path();
    fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let _ = launchctl(&["bootout", &gui_domain(), &path.to_string_lossy()]);
    fs::write(&path, plist).map_err(|e| e.to_string())?;
    launchctl(&["bootstrap", &gui_domain(), &path.to_string_lossy()])?;
    println!("installed {} (logs: {})", path.display(), log.display());
    Ok(())
}

fn uninstall() -> Result<(), String> {
    let path = plist_path();
    let _ = launchctl(&["bootout", &gui_domain(), &path.to_string_lossy()]);
    if path.exists() {
        fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    println!("uninstalled");
    Ok(())
}

fn gui_domain() -> String {
    let uid = Command::new("id").arg("-u").output().map(|o| o.stdout).unwrap_or_default();
    format!("gui/{}", String::from_utf8_lossy(&uid).trim())
}

fn launchctl(args: &[&str]) -> Result<(), String> {
    let out = Command::new("launchctl").args(args).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("launchctl {}: {}", args[0], String::from_utf8_lossy(&out.stderr).trim()))
    }
}

// ---------------------------------------------------------------- cli

const USAGE: &str = "\
duskpaper — light/dark wallpaper switcher

USAGE:
    duskpaper set <light-image> <dark-image>   save images and apply now
    duskpaper apply                            apply once for current mode
    duskpaper watch                            run in foreground, follow changes
    duskpaper install                          run at login (LaunchAgent)
    duskpaper uninstall                        remove LaunchAgent
    duskpaper status                           show config and current mode";

fn main() -> ExitCode {
    let mtm = MainThreadMarker::new().expect("must run on main thread");
    let args: Vec<String> = env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let result = match args.as_slice() {
        ["set", light, dark] => (|| {
            let cfg = Config { light: absolute_image(light)?, dark: absolute_image(dark)? };
            save_config(&cfg)?;
            println!("saved {}", config_path().display());
            apply(mtm)
        })(),
        ["apply"] => apply(mtm),
        ["watch"] => watch(mtm),
        ["install"] => install(),
        ["uninstall"] => uninstall(),
        ["status"] => (|| {
            let cfg = load_config()?;
            println!("mode:   {}", if is_dark() { "dark" } else { "light" });
            println!("light:  {}", cfg.light.display());
            println!("dark:   {}", cfg.dark.display());
            println!("agent:  {}", if plist_path().exists() { "installed" } else { "not installed" });
            Ok(())
        })(),
        _ => {
            println!("{USAGE}");
            return if args.is_empty() || args == ["help"] || args == ["--help"] {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
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
