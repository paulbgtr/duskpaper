//! Platform core: config, appearance detection, wallpaper setting, change
//! observers, thumbnails and the LaunchAgent.

use std::{
    collections::{BTreeMap, hash_map::DefaultHasher},
    env, fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Command,
    ptr::NonNull,
    rc::Rc,
};

use block2::RcBlock;
use objc2::{MainThreadMarker, rc::Retained, runtime::NSObjectProtocol, runtime::ProtocolObject};
use objc2_app_kit::{
    NSScreen, NSWorkspace, NSWorkspaceActiveSpaceDidChangeNotification,
    NSWorkspaceDidWakeNotification, NSWorkspaceScreensDidWakeNotification,
};
use objc2_foundation::{
    NSDictionary, NSDistributedNotificationCenter, NSNotification, NSString, NSURL, NSUserDefaults,
};

pub const LABEL: &str = "dev.duskpaper.agent";

// ---------------------------------------------------------------- config

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Light,
    Dark,
}

impl Mode {
    pub fn key(self) -> &'static str {
        match self {
            Mode::Light => "light",
            Mode::Dark => "dark",
        }
    }
}

#[derive(Clone, Default, Debug)]
pub struct Pair {
    pub light: Option<PathBuf>,
    pub dark: Option<PathBuf>,
}

impl Pair {
    pub fn get(&self, mode: Mode) -> Option<&PathBuf> {
        match mode {
            Mode::Light => self.light.as_ref(),
            Mode::Dark => self.dark.as_ref(),
        }
    }
    fn slot(&mut self, mode: Mode) -> &mut Option<PathBuf> {
        match mode {
            Mode::Light => &mut self.light,
            Mode::Dark => &mut self.dark,
        }
    }
}

/// `light=` / `dark=` apply to every display; `light@<display>=` overrides one.
#[derive(Clone, Default, Debug)]
pub struct Config {
    pub default: Pair,
    pub screens: BTreeMap<String, Pair>,
}

impl Config {
    pub fn resolve(&self, screen: &str, mode: Mode) -> Option<&PathBuf> {
        self.screens
            .get(screen)
            .and_then(|p| p.get(mode))
            .or_else(|| self.default.get(mode))
    }

    pub fn set(&mut self, screen: Option<&str>, mode: Mode, image: Option<PathBuf>) {
        let pair = match screen {
            Some(s) => self.screens.entry(s.to_string()).or_default(),
            None => &mut self.default,
        };
        *pair.slot(mode) = image;
    }

    pub fn is_empty(&self) -> bool {
        self.default.light.is_none()
            && self.default.dark.is_none()
            && self.screens.values().all(|p| p.light.is_none() && p.dark.is_none())
    }
}

pub fn home() -> PathBuf {
    PathBuf::from(env::var("HOME").expect("HOME is not set"))
}

pub fn config_path() -> PathBuf {
    home().join(".config/duskpaper/config")
}

pub fn cache_dir() -> PathBuf {
    home().join("Library/Caches/duskpaper")
}

/// Missing config file is an empty config.
pub fn load_config() -> Config {
    let mut cfg = Config::default();
    let Ok(text) = fs::read_to_string(config_path()) else {
        return cfg;
    };
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let (mode, screen) = match key.split_once('@') {
            Some((m, s)) => (m, Some(s)),
            None => (key, None),
        };
        let mode = match mode.trim() {
            "light" => Mode::Light,
            "dark" => Mode::Dark,
            _ => continue,
        };
        cfg.set(screen, mode, Some(PathBuf::from(value)));
    }
    cfg
}

pub fn save_config(cfg: &Config) -> Result<(), String> {
    let mut text = String::new();
    let mut write = |prefix: &str, pair: &Pair| {
        for mode in [Mode::Light, Mode::Dark] {
            if let Some(p) = pair.get(mode) {
                text.push_str(&format!("{}{prefix}={}\n", mode.key(), p.display()));
            }
        }
    };
    write("", &cfg.default);
    for (screen, pair) in &cfg.screens {
        write(&format!("@{screen}"), pair);
    }
    let path = config_path();
    fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    fs::write(&path, text).map_err(|e| e.to_string())
}

pub fn absolute_image(arg: &str) -> Result<PathBuf, String> {
    let p = Path::new(arg).canonicalize().map_err(|e| format!("{arg}: {e}"))?;
    if !p.is_file() {
        return Err(format!("{} is not a file", p.display()));
    }
    Ok(p)
}

// ---------------------------------------------------------------- macOS

pub fn current_mode() -> Mode {
    let style = NSUserDefaults::standardUserDefaults()
        .stringForKey(&NSString::from_str("AppleInterfaceStyle"));
    if style.is_some_and(|s| s.to_string().eq_ignore_ascii_case("dark")) {
        Mode::Dark
    } else {
        Mode::Light
    }
}

/// Display names, made unique when two monitors share a name.
pub fn screen_names(mtm: MainThreadMarker) -> Vec<String> {
    screens_named(mtm).into_iter().map(|(name, _)| name).collect()
}

fn screens_named(mtm: MainThreadMarker) -> Vec<(String, Retained<NSScreen>)> {
    let mut out: Vec<(String, Retained<NSScreen>)> = Vec::new();
    for screen in NSScreen::screens(mtm).iter() {
        let base = screen.localizedName().to_string();
        let mut name = base.clone();
        let mut n = 2;
        while out.iter().any(|(existing, _)| *existing == name) {
            name = format!("{base} {n}");
            n += 1;
        }
        out.push((name, screen));
    }
    out
}

/// Set the configured image for the current mode on every display (current
/// Space). Returns a line per display that changed.
pub fn apply(mtm: MainThreadMarker) -> Result<Vec<String>, String> {
    let cfg = load_config();
    if cfg.is_empty() {
        return Err(format!(
            "no images configured ({}) — run `duskpaper set <light> <dark>` or open the app",
            config_path().display()
        ));
    }
    let mode = current_mode();
    let workspace = NSWorkspace::sharedWorkspace();
    let options = NSDictionary::new();
    let mut changed = Vec::new();
    for (name, screen) in screens_named(mtm) {
        let Some(image) = cfg.resolve(&name, mode) else { continue };
        let url = NSURL::fileURLWithPath(&NSString::from_str(&image.to_string_lossy()));
        if workspace.desktopImageURLForScreen(&screen).is_some_and(|c| c.isEqual(Some(&url))) {
            continue;
        }
        // SAFETY: empty options dictionary, valid file URL and screen.
        unsafe { workspace.setDesktopImageURL_forScreen_options_error(&url, &screen, &options) }
            .map_err(|e| format!("{name}: {}", e.localizedDescription()))?;
        changed.push(format!("{name}: {} → {}", mode.key(), image.display()));
    }
    Ok(changed)
}

pub fn apply_and_log(mtm: MainThreadMarker) {
    match apply(mtm) {
        Ok(lines) => lines.iter().for_each(|l| println!("{l}")),
        Err(e) => eprintln!("duskpaper: {e}"),
    }
}

pub type Observer = Retained<ProtocolObject<dyn NSObjectProtocol>>;

/// Call `on_change` (on the main thread) when the appearance flips, the Space
/// changes, or the Mac wakes. Keep the returned observers alive.
pub fn observe_changes(on_change: impl Fn() + 'static) -> Vec<Observer> {
    let on_change = Rc::new(on_change);
    let handler = RcBlock::new(move |_n: NonNull<NSNotification>| on_change());
    let mut observers = Vec::new();
    unsafe {
        observers.push(
            NSDistributedNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
                Some(&NSString::from_str("AppleInterfaceThemeChangedNotification")),
                None,
                None,
                &handler,
            ),
        );
        let ws = NSWorkspace::sharedWorkspace().notificationCenter();
        for name in [
            NSWorkspaceActiveSpaceDidChangeNotification,
            NSWorkspaceDidWakeNotification,
            NSWorkspaceScreensDidWakeNotification,
        ] {
            observers.push(ws.addObserverForName_object_queue_usingBlock(
                Some(name),
                None,
                None,
                &handler,
            ));
        }
    }
    observers
}

/// Flip system Light/Dark. First use asks for Automation permission.
pub fn toggle_system_appearance() {
    std::thread::spawn(|| {
        let out = Command::new("osascript")
            .args([
                "-e",
                "tell application \"System Events\" to tell appearance preferences to set dark mode to not dark mode",
            ])
            .output();
        if let Ok(o) = out
            && !o.status.success()
        {
            eprintln!("duskpaper: toggle failed: {}", String::from_utf8_lossy(&o.stderr).trim());
        }
    });
}

/// PNG thumbnail (via `sips`, which also reads HEIC) cached by path + mtime.
pub fn thumbnail(image: &Path) -> Option<PathBuf> {
    let mtime = fs::metadata(image).ok()?.modified().ok()?;
    let mut h = DefaultHasher::new();
    image.hash(&mut h);
    mtime.hash(&mut h);
    let dir = cache_dir().join("thumbs");
    let out = dir.join(format!("{:016x}.png", h.finish()));
    if out.exists() {
        return Some(out);
    }
    fs::create_dir_all(&dir).ok()?;
    let ok = Command::new("sips")
        .args(["-s", "format", "png", "-Z", "720"])
        .arg(image)
        .arg("--out")
        .arg(&out)
        .output()
        .is_ok_and(|o| o.status.success());
    ok.then_some(out)
}

// ---------------------------------------------------------------- launch agent

pub fn plist_path() -> PathBuf {
    home().join(format!("Library/LaunchAgents/{LABEL}.plist"))
}

pub fn agent_installed() -> bool {
    plist_path().exists()
}

/// Write the LaunchAgent that runs the menu bar app at login. With `load`,
/// also (re)start it now.
pub fn install_agent(load: bool) -> Result<PathBuf, String> {
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
    <array><string>{}</string><string>app</string><string>--hidden</string></array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
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
    if load {
        let _ = launchctl(&["bootout", &gui_domain(), &path.to_string_lossy()]);
    }
    fs::write(&path, plist).map_err(|e| e.to_string())?;
    if load {
        launchctl(&["bootstrap", &gui_domain(), &path.to_string_lossy()])?;
    }
    Ok(log)
}

/// Remove the LaunchAgent. With `unload`, also stop the running agent.
pub fn uninstall_agent(unload: bool) -> Result<(), String> {
    let path = plist_path();
    if unload {
        let _ = launchctl(&["bootout", &gui_domain(), &path.to_string_lossy()]);
    }
    if path.exists() {
        fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
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
