//! AppKit status-item backend. All AppKit objects are created, touched, and
//! released on eframe's winit main thread; only plain tray events cross threads.

use std::sync::mpsc::Sender;

use eframe::egui;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSEventMask, NSEventModifierFlags, NSEventType, NSImage, NSScreen, NSStatusBar,
    NSStatusItem,
};
use objc2_foundation::{NSData, NSObject, NSObjectProtocol, NSSize, NSString};

use super::{TrayEvent, emit};

const STATUS_ICON: &[u8] = include_bytes!("../assets/branding/bs-podcasts-shell-icon-32.png");

pub(super) struct MacTray {
    status_bar: Option<Retained<NSStatusBar>>,
    status_item: Option<Retained<NSStatusItem>>,
    target: Option<Retained<TrayTarget>>,
    main_thread: Option<MainThreadMarker>,
}

struct TrayTargetIvars {
    status_item: Retained<NSStatusItem>,
    ctx: egui::Context,
    events: Sender<TrayEvent>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = TrayTargetIvars]
    struct TrayTarget;

    unsafe impl NSObjectProtocol for TrayTarget {}

    impl TrayTarget {
        #[unsafe(method(statusItemClicked:))]
        fn status_item_clicked(&self, _sender: &AnyObject) {
            let ivars = self.ivars();
            let current_event = NSApplication::sharedApplication(self.mtm()).currentEvent();
            let open_menu = current_event.as_ref().is_some_and(|event| {
                event.r#type() == NSEventType::RightMouseUp
                    || event.modifierFlags().contains(NSEventModifierFlags::Control)
            });
            let event = if open_menu {
                status_menu_event(&ivars.status_item, self.mtm(), &ivars.ctx)
                    .unwrap_or(TrayEvent::Restore)
            } else {
                TrayEvent::Restore
            };
            emit(&ivars.ctx, &ivars.events, event);
        }
    }
);

impl TrayTarget {
    fn new(mtm: MainThreadMarker, ivars: TrayTargetIvars) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ivars);
        // SAFETY: `NSObject::init` is the correct initializer for this target class.
        unsafe { msg_send![super(this), init] }
    }
}

impl MacTray {
    pub(super) fn new(ctx: egui::Context, events: Sender<TrayEvent>) -> Self {
        let Some(mtm) = MainThreadMarker::new() else {
            emit_unavailable(
                &ctx,
                &events,
                "macOS tray creation was not running on the AppKit main thread.",
            );
            return Self::empty();
        };

        let status_bar = NSStatusBar::systemStatusBar();
        let status_item = status_bar.statusItemWithLength(-2.0);
        let Some(button) = status_item.button(mtm) else {
            status_bar.removeStatusItem(&status_item);
            emit_unavailable(
                &ctx,
                &events,
                "macOS did not provide a button for the system tray item.",
            );
            return Self::empty();
        };

        let Some(image) = status_image() else {
            status_bar.removeStatusItem(&status_item);
            emit_unavailable(
                &ctx,
                &events,
                "The embedded BS Player macOS tray icon could not be loaded.",
            );
            return Self::empty();
        };
        image.setSize(NSSize::new(16.0, 16.0));
        button.setImage(Some(&image));

        let target = TrayTarget::new(
            mtm,
            TrayTargetIvars {
                status_item: status_item.clone(),
                ctx: ctx.clone(),
                events: events.clone(),
            },
        );
        let target_object: &AnyObject = target.as_ref();
        // SAFETY: target is the retained MainThreadOnly action receiver and the selector's
        // one-object signature matches `status_item_clicked` above.
        unsafe {
            button.setTarget(Some(target_object));
            button.setAction(Some(objc2::sel!(statusItemClicked:)));
        }
        let _ = button.sendActionOn(NSEventMask::LeftMouseUp | NSEventMask::RightMouseUp);
        button.setToolTip(Some(&NSString::from_str("BS Player")));
        status_item.setVisible(true);

        if !status_item.isVisible() {
            unsafe {
                button.setTarget(None);
                button.setAction(None);
            }
            status_bar.removeStatusItem(&status_item);
            emit_unavailable(
                &ctx,
                &events,
                "The macOS system tray item did not become visible.",
            );
            return Self::empty();
        }

        emit(
            &ctx,
            &events,
            TrayEvent::AvailabilityChanged {
                available: true,
                reason: String::new(),
            },
        );
        Self {
            status_bar: Some(status_bar),
            status_item: Some(status_item),
            target: Some(target),
            main_thread: Some(mtm),
        }
    }

    fn empty() -> Self {
        Self {
            status_bar: None,
            status_item: None,
            target: None,
            main_thread: None,
        }
    }

    pub(super) fn update_tooltip(&self, tooltip: &str) {
        let (Some(item), Some(mtm)) = (&self.status_item, self.main_thread) else {
            return;
        };
        if let Some(button) = item.button(mtm) {
            button.setToolTip(Some(&NSString::from_str(tooltip)));
        }
    }

    pub(super) fn shutdown(&mut self) {
        if let (Some(status_bar), Some(status_item), Some(mtm)) =
            (&self.status_bar, &self.status_item, self.main_thread)
        {
            if let Some(button) = status_item.button(mtm) {
                // SAFETY: clear the weak target and selector while both are still retained here.
                unsafe {
                    button.setTarget(None);
                    button.setAction(None);
                }
            }
            status_item.setVisible(false);
            status_bar.removeStatusItem(status_item);
        }
        drop(self.target.take());
        drop(self.status_item.take());
        drop(self.status_bar.take());
        self.main_thread = None;
    }
}

impl Drop for MacTray {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn status_image() -> Option<Retained<NSImage>> {
    // NSData copies this static, manifest-tracked PNG during construction.
    let data = unsafe {
        NSData::dataWithBytes_length(STATUS_ICON.as_ptr().cast(), STATUS_ICON.len() as _)
    };
    NSImage::initWithData(NSImage::alloc(), &data)
}

fn emit_unavailable(ctx: &egui::Context, events: &Sender<TrayEvent>, reason: &str) {
    emit(
        ctx,
        events,
        TrayEvent::AvailabilityChanged {
            available: false,
            reason: reason.into(),
        },
    );
}

/// Convert AppKit's global bottom-left point rectangles into Egui's global
/// top-left point space. AppKit and Egui both use points here; only egui zoom
/// is divided out (never the native Retina pixel scale).
fn status_menu_event(
    status_item: &NSStatusItem,
    mtm: MainThreadMarker,
    ctx: &egui::Context,
) -> Option<TrayEvent> {
    let button = status_item.button(mtm)?;
    let window = button.window()?;
    let primary = NSScreen::screens(mtm).firstObject()?;
    let primary_frame = primary.frame();
    let primary_top = primary_frame.origin.y + primary_frame.size.height;
    let screen = window.screen().unwrap_or_else(|| primary.clone());
    let visible = screen.visibleFrame();
    let button_window_rect = button.convertRect_toView(button.bounds(), None);
    let button_screen_rect = window.convertRectToScreen(button_window_rect);

    let zoom = ctx.zoom_factor().max(0.1) as f64;
    let x = button_screen_rect.origin.x + button_screen_rect.size.width;
    let y = primary_top - button_screen_rect.origin.y - button_screen_rect.size.height;
    let button_bottom = primary_top - button_screen_rect.origin.y;
    let visible_top = primary_top - visible.origin.y - visible.size.height;
    let bounds = [
        (visible.origin.x / zoom) as f32,
        (visible_top.max(button_bottom) / zoom) as f32,
        ((visible.origin.x + visible.size.width) / zoom) as f32,
        ((primary_top - visible.origin.y) / zoom) as f32,
    ];
    Some(TrayEvent::OpenMenu {
        position: [(x / zoom) as f32, (y / zoom) as f32],
        work_area: Some(bounds),
    })
}
