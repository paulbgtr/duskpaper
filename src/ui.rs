//! GPUI window + menu bar app.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    time::Duration,
};

use futures::StreamExt;
use gpui::{
    App, Application, Bounds, Context, ExternalPaths, Hsla, KeyBinding, ObjectFit, PathPromptOptions,
    SharedString, Subscription, TitlebarOptions, Window, WindowAppearance,
    WindowBackgroundAppearance, WindowBounds, WindowOptions, actions, div, img, point, prelude::*,
    px, rgba, size,
};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

use crate::{
    core::{self, Config, Mode},
    statusbar::{self, Cmd},
};

actions!(duskpaper, [CloseWindow, Quit]);

pub fn run(hidden: bool) {
    Application::new().run(move |cx: &mut App| {
        let mtm = MainThreadMarker::new().expect("main thread");

        // Menu bar app: no Dock icon (DUSKPAPER_DOCK=1 keeps it, for debugging).
        if std::env::var_os("DUSKPAPER_DOCK").is_none() {
            NSApplication::sharedApplication(mtm)
                .setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        }

        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        statusbar::install(mtm, tx);
        std::mem::forget(core::observe_changes(|| statusbar::send(Cmd::Refresh)));
        core::apply_and_log(mtm);

        cx.bind_keys([
            KeyBinding::new("cmd-w", CloseWindow, None),
            KeyBinding::new("cmd-q", Quit, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());

        cx.spawn(async move |cx| {
            while let Some(cmd) = rx.next().await {
                match cmd {
                    Cmd::Open => {
                        let _ = cx.update(open_window);
                    }
                    Cmd::ToggleAppearance => core::toggle_system_appearance(),
                    Cmd::Refresh => {
                        // The defaults write can land a moment after the notification.
                        cx.background_executor().timer(Duration::from_millis(250)).await;
                        let _ = cx.update(|cx| {
                            core::apply_and_log(mtm);
                            cx.refresh_windows();
                        });
                    }
                    Cmd::Quit => {
                        let _ = cx.update(|cx| cx.quit());
                    }
                }
            }
        })
        .detach();

        if !hidden {
            open_window(cx);
        }
    });
}

fn open_window(cx: &mut App) {
    cx.activate(true);
    if let Some(handle) = cx.windows().into_iter().next() {
        let _ = handle.update(cx, |_, window, _| window.activate_window());
        return;
    }
    let bounds = Bounds::centered(None, size(px(620.), px(352.)), cx);
    let _ = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("duskpaper".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(14.), px(15.))),
            }),
            window_background: WindowBackgroundAppearance::Blurred,
            is_resizable: false,
            is_minimizable: false,
            focus: true,
            show: true,
            ..Default::default()
        },
        |window, cx| cx.new(|cx| Duskpaper::new(window, cx)),
    );
}

// ---------------------------------------------------------------- theme

struct Theme {
    bg: Hsla,
    card: Hsla,
    card_hover: Hsla,
    well: Hsla,
    seg: Hsla,
    seg_active: Hsla,
    text: Hsla,
    muted: Hsla,
    accent: Hsla,
    track: Hsla,
}

impl Theme {
    fn new(dark: bool) -> Self {
        if dark {
            Theme {
                bg: rgba(0x1c1b1fe0).into(),
                card: rgba(0xffffff0d).into(),
                card_hover: rgba(0xffffff17).into(),
                well: rgba(0x00000040).into(),
                seg: rgba(0xffffff12).into(),
                seg_active: rgba(0xffffff26).into(),
                text: rgba(0xf4f2f7ff).into(),
                muted: rgba(0xa29fabff).into(),
                accent: rgba(0xff9f43ff).into(),
                track: rgba(0xffffff2a).into(),
            }
        } else {
            Theme {
                bg: rgba(0xf6f4f1e6).into(),
                card: rgba(0xffffffb3).into(),
                card_hover: rgba(0xffffffff).into(),
                well: rgba(0x0000000d).into(),
                seg: rgba(0x0000000f).into(),
                seg_active: rgba(0xffffffff).into(),
                text: rgba(0x1d1b20ff).into(),
                muted: rgba(0x6f6a78ff).into(),
                accent: rgba(0xe8730cff).into(),
                track: rgba(0x00000024).into(),
            }
        }
    }
}

// ---------------------------------------------------------------- view

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Screen(usize),
    Settings,
}

struct Duskpaper {
    screens: Vec<String>,
    tab: Tab,
    config: Config,
    /// image → thumbnail (None = no preview possible)
    thumbs: HashMap<PathBuf, Option<PathBuf>>,
    loading: HashSet<PathBuf>,
    login: bool,
    status: Option<SharedString>,
    _appearance: Subscription,
}

impl Duskpaper {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mtm = MainThreadMarker::new().expect("main thread");
        Self {
            screens: core::screen_names(mtm),
            tab: Tab::Screen(0),
            config: core::load_config(),
            thumbs: HashMap::new(),
            loading: HashSet::new(),
            login: core::agent_installed(),
            status: None,
            _appearance: cx.observe_window_appearance(window, |_, _, cx| cx.notify()),
        }
    }

    fn screen(&self) -> Option<&str> {
        match self.tab {
            Tab::Screen(i) => self.screens.get(i).map(String::as_str),
            Tab::Settings => None,
        }
    }

    fn set_image(&mut self, mode: Mode, image: PathBuf, cx: &mut Context<Self>) {
        let Some(screen) = self.screen().map(str::to_owned) else { return };
        if self.screens.len() == 1 {
            // One display: store as the default so new displays inherit it.
            self.config.set(None, mode, Some(image));
            self.config.set(Some(&screen), mode, None);
        } else {
            self.config.set(Some(&screen), mode, Some(image));
        }
        self.status = match core::save_config(&self.config) {
            Err(e) => Some(format!("Couldn't save: {e}").into()),
            Ok(()) => {
                let mtm = MainThreadMarker::new().expect("main thread");
                match core::apply(mtm) {
                    Err(e) => Some(e.into()),
                    Ok(_) => None,
                }
            }
        };
        cx.notify();
    }

    fn pick(&mut self, mode: Mode, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(format!("Use for {} mode", mode.key()).into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update(cx, |this, cx| this.set_image(mode, path, cx));
            }
        })
        .detach();
    }

    fn thumb(&mut self, image: &PathBuf, cx: &mut Context<Self>) -> Option<Option<PathBuf>> {
        if let Some(t) = self.thumbs.get(image) {
            return Some(t.clone());
        }
        if self.loading.insert(image.clone()) {
            let image = image.clone();
            cx.spawn(async move |this, cx| {
                let src = image.clone();
                let thumb = cx.background_executor().spawn(async move { core::thumbnail(&src) }).await;
                let _ = this.update(cx, |this, cx| {
                    this.loading.remove(&image);
                    this.thumbs.insert(image, thumb);
                    cx.notify();
                });
            })
            .detach();
        }
        None
    }

    fn toggle_login(&mut self, cx: &mut Context<Self>) {
        let result = if self.login {
            core::uninstall_agent(false)
        } else {
            core::install_agent(false).map(|_| ())
        };
        match result {
            Ok(()) => {
                self.login = !self.login;
                self.status = None;
            }
            Err(e) => self.status = Some(e.into()),
        }
        cx.notify();
    }
}

impl Render for Duskpaper {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dark = matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let t = Theme::new(dark);

        let body = match self.tab {
            Tab::Screen(_) => self.render_cards(dark, &t, cx).into_any_element(),
            Tab::Settings => self.render_settings(&t, cx).into_any_element(),
        };

        let footer: SharedString = self.status.clone().unwrap_or_else(|| match self.tab {
            Tab::Screen(_) if self.screens.len() > 1 => {
                "Click a card or drop an image on it. Applies to this display only.".into()
            }
            Tab::Screen(_) => "Click a card or drop an image on it.".into(),
            Tab::Settings => format!("duskpaper {}", env!("CARGO_PKG_VERSION")).into(),
        });

        div()
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .size_full()
            .flex()
            .flex_col()
            .bg(t.bg)
            .text_color(t.text)
            .font_family(".SystemUIFont")
            .text_sm()
            .child(self.render_header(dark, &t))
            .child(self.render_tabs(&t, cx))
            .child(div().flex_1().px_4().pt_3().child(body))
            .child(
                div()
                    .px_4()
                    .pb_3()
                    .text_xs()
                    .text_color(if self.status.is_some() { t.accent } else { t.muted })
                    .truncate()
                    .child(footer),
            )
    }
}

impl Duskpaper {
    fn render_header(&self, dark: bool, t: &Theme) -> impl IntoElement {
        div()
            .h(px(44.))
            .flex_none()
            .flex()
            .items_center()
            .pl(px(84.))
            .pr_4()
            .child(div().flex_1().font_weight(gpui::FontWeight::SEMIBOLD).child("duskpaper"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_base()
                    .text_color(t.muted)
                    .child("☀︎")
                    .child(
                        switch("appearance", dark, t)
                            .on_click(|_, _, _| core::toggle_system_appearance()),
                    )
                    .child("☾"),
            )
    }

    fn render_tabs(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let mut tabs: Vec<(Tab, SharedString)> = self
            .screens
            .iter()
            .enumerate()
            .map(|(i, name)| (Tab::Screen(i), name.clone().into()))
            .collect();
        tabs.push((Tab::Settings, "Settings".into()));

        div().flex().justify_center().child(
            div()
                .flex()
                .gap_0p5()
                .p_0p5()
                .rounded_lg()
                .bg(t.seg)
                .children(tabs.into_iter().enumerate().map(|(i, (tab, label))| {
                    let active = self.tab == tab;
                    div()
                        .id(("tab", i))
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .text_xs()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .cursor_pointer()
                        .when(active, |d| d.bg(t.seg_active).shadow_sm())
                        .when(!active, |d| d.text_color(t.muted))
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.tab = tab;
                            this.status = None;
                            cx.notify();
                        }))
                })),
        )
    }

    fn render_cards(&mut self, dark: bool, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let screen = self.screen().unwrap_or_default().to_owned();
        let now = if dark { Mode::Dark } else { Mode::Light };
        let cards: Vec<_> = [Mode::Light, Mode::Dark]
            .into_iter()
            .map(|mode| {
                let image = self.config.resolve(&screen, mode).cloned();
                let thumb = image.as_ref().map(|i| self.thumb(i, cx));
                self.render_card(mode, mode == now, image, thumb, t, cx)
            })
            .collect();
        div().flex().gap_4().children(cards)
    }

    fn render_card(
        &self,
        mode: Mode,
        active: bool,
        image: Option<PathBuf>,
        thumb: Option<Option<Option<PathBuf>>>,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let accent = t.accent;
        let preview = div()
            .w_full()
            .h(px(160.))
            .rounded_lg()
            .overflow_hidden()
            .bg(t.well)
            .flex()
            .items_center()
            .justify_center()
            .text_xs()
            .text_color(t.muted);
        let preview = match thumb {
            None => preview.child("Choose an image…"),
            Some(None) => preview.child("Loading…"),
            Some(Some(None)) => preview.child("No preview"),
            // Absolutely positioned so the image's natural size never affects layout.
            Some(Some(Some(path))) => preview.relative().child(
                img(path).absolute().inset_0().size_full().object_fit(ObjectFit::Cover),
            ),
        };
        let file_name: SharedString = image
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
            .into();

        div()
            .id(mode.key())
            .flex_1()
            .flex_basis(px(0.))
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .rounded_xl()
            .bg(t.card)
            .border_2()
            .border_color(if active { t.accent } else { gpui::transparent_black() })
            .cursor_pointer()
            .hover(|s| s.bg(t.card_hover))
            .drag_over::<ExternalPaths>(move |s, _, _, _| s.border_color(accent))
            .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                if let Some(path) = paths.paths().first() {
                    this.set_image(mode, path.clone(), cx);
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| this.pick(mode, cx)))
            .child(preview)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_1()
                    .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(match mode {
                        Mode::Light => "Light",
                        Mode::Dark => "Dark",
                    }))
                    .when(active, |d| {
                        d.child(
                            div()
                                .px_1p5()
                                .rounded_full()
                                .bg(t.accent)
                                .text_color(gpui::white())
                                .text_xs()
                                .child("now"),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .justify_end()
                            .text_xs()
                            .text_color(t.muted)
                            .child(div().truncate().child(file_name)),
                    ),
            )
    }

    fn render_settings(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let row = |label: &'static str, detail: &'static str| {
            div()
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .py_2p5()
                .rounded_lg()
                .bg(t.card)
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(div().font_weight(gpui::FontWeight::MEDIUM).child(label))
                        .child(div().text_xs().text_color(t.muted).child(detail)),
                )
        };
        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .px_3()
                .py_1()
                .rounded_md()
                .bg(t.seg)
                .text_xs()
                .cursor_pointer()
                .hover(|s| s.bg(t.seg_active))
                .child(label)
        };

        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                row("Start at login", "Keeps duskpaper in the menu bar after you log in.").child(
                    switch("login", self.login, t)
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_login(cx))),
                ),
            )
            .child(
                row("Apply now", "Re-set the wallpaper for the current mode on every display.")
                    .child(button("apply", "Apply").on_click(cx.listener(|this, _, _, cx| {
                        let mtm = MainThreadMarker::new().expect("main thread");
                        this.status = Some(match core::apply(mtm) {
                            Ok(lines) if lines.is_empty() => "Already up to date.".into(),
                            Ok(lines) => format!("Updated {} display(s).", lines.len()).into(),
                            Err(e) => e.into(),
                        });
                        cx.notify();
                    }))),
            )
            .child(
                row("Config file", "~/.config/duskpaper/config").child(
                    button("reveal", "Show in Finder").on_click(|_, _, _| {
                        let _ = std::process::Command::new("open")
                            .arg("-R")
                            .arg(core::config_path())
                            .spawn();
                    }),
                ),
            )
    }
}

fn switch(id: &'static str, on: bool, t: &Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .w(px(36.))
        .h(px(20.))
        .p(px(2.))
        .rounded_full()
        .flex()
        .cursor_pointer()
        .bg(if on { t.accent } else { t.track })
        .when(on, |d| d.justify_end())
        .child(div().size(px(16.)).rounded_full().bg(gpui::white()).shadow_sm())
}
