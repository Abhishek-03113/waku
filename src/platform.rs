use gpui::Window;

// crates.io GPUI 0.2.2 leaves `start_window_move` as a no-op on macOS.
#[cfg(target_os = "macos")]
pub fn start_window_move(window: &Window) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSView};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };

    // GPUI owns this NSView for the lifetime of `window`, and AppKit access is
    // guarded by the main-thread marker above.
    unsafe {
        let view = handle.ns_view.cast::<NSView>().as_ref();
        let Some(native_window) = view.window() else {
            return;
        };
        let app = NSApplication::sharedApplication(main_thread);
        let Some(event) = app.currentEvent() else {
            return;
        };
        native_window.performWindowDragWithEvent(&event);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn start_window_move(window: &Window) {
    window.start_window_move();
}

/// Give GPUI's window-wide blur view the semantic material macOS uses for
/// sidebars. Opaque Waku surfaces cover the effect everywhere else.
#[cfg(target_os = "macos")]
pub fn configure_sidebar_material(window: &Window) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState,
        NSVisualEffectView,
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    let Some(_main_thread) = MainThreadMarker::new() else {
        return;
    };

    // GPUI owns the view hierarchy and creates the effect view before the
    // root entity is installed. We only adjust public AppKit properties.
    unsafe {
        let view = handle.ns_view.cast::<NSView>().as_ref();
        let Some(native_window) = view.window() else {
            return;
        };
        let Some(content_view) = native_window.contentView() else {
            return;
        };

        for subview in content_view.subviews().iter() {
            let Some(effect_view) = subview.downcast_ref::<NSVisualEffectView>() else {
                continue;
            };
            effect_view.setMaterial(NSVisualEffectMaterial::Sidebar);
            effect_view.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
            effect_view.setState(NSVisualEffectState::Active);
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn configure_sidebar_material(_: &Window) {}
