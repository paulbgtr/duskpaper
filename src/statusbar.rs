//! Menu bar icon (NSStatusItem) whose menu sends `Cmd`s to the app.

use std::sync::OnceLock;

use futures::channel::mpsc::UnboundedSender;
use objc2::{
    MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained,
    runtime::AnyObject, sel,
};
use objc2_app_kit::{NSImage, NSMenu, NSMenuItem, NSStatusBar, NSVariableStatusItemLength};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};

#[derive(Debug, Clone, Copy)]
pub enum Cmd {
    Open,
    ToggleAppearance,
    Refresh,
    Quit,
}

static SENDER: OnceLock<UnboundedSender<Cmd>> = OnceLock::new();

pub fn send(cmd: Cmd) {
    if let Some(tx) = SENDER.get() {
        let _ = tx.unbounded_send(cmd);
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "DuskpaperMenuTarget"]
    struct MenuTarget;

    unsafe impl NSObjectProtocol for MenuTarget {}

    impl MenuTarget {
        #[unsafe(method(open:))]
        fn open(&self, _sender: Option<&AnyObject>) {
            send(Cmd::Open);
        }

        #[unsafe(method(toggle:))]
        fn toggle(&self, _sender: Option<&AnyObject>) {
            send(Cmd::ToggleAppearance);
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            send(Cmd::Quit);
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

/// Install the status item. It lives for the rest of the process.
pub fn install(mtm: MainThreadMarker, tx: UnboundedSender<Cmd>) {
    let _ = SENDER.set(tx);
    let target = MenuTarget::new(mtm);

    let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    if let Some(button) = item.button(mtm) {
        let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str("circle.lefthalf.filled"),
            Some(&NSString::from_str("duskpaper")),
        );
        if let Some(image) = image {
            image.setTemplate(true);
            button.setImage(Some(&image));
        } else {
            button.setTitle(&NSString::from_str("◐"));
        }
    }

    let menu = NSMenu::new(mtm);
    for (title, action, key) in [
        ("Open duskpaper…", sel!(open:), ""),
        ("Toggle Light / Dark", sel!(toggle:), ""),
        ("-", sel!(open:), ""),
        ("Quit duskpaper", sel!(quit:), "q"),
    ] {
        if title == "-" {
            menu.addItem(&NSMenuItem::separatorItem(mtm));
            continue;
        }
        let mi = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                Some(action),
                &NSString::from_str(key),
            )
        };
        unsafe { mi.setTarget(Some(&target)) };
        menu.addItem(&mi);
    }
    item.setMenu(Some(&menu));

    // Keep both alive for the lifetime of the process.
    std::mem::forget(item);
    std::mem::forget(target);
}
