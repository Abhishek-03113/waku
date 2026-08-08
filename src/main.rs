#![recursion_limit = "256"]

mod amp_session;
mod app;
mod assets;
mod blob_store;
mod browser;
mod checkpoint;
mod claude_session;
mod command_env;
mod composer_complete;
mod computer_use;
mod cursor_session;
mod driver;
mod grok_session;
mod identity;
mod input;
mod md;
mod model;
mod model_catalog;
mod opencode_session;
mod persistence;
mod platform;
mod projectless;
mod query;
mod terminal;
mod theme;
mod ui;
mod updater;
mod usage;

use gpui::{
    App, Application, Bounds, KeyBinding, Menu, MenuItem, TitlebarOptions,
    WindowBackgroundAppearance, WindowBounds, WindowOptions, actions, point, px, size,
};

use crate::app::Waku;
use crate::identity::APP_NAME;
actions!(
    waku,
    [
        Quit,
        CloseWindow,
        NewSession,
        OpenSettings,
        CheckForUpdates,
        ToggleSidebar,
        ToggleRightPanel,
        ToggleFpsCounter,
        NavigateBack,
        NavigateForward,
        FocusComposer,
        ToggleModelPicker,
        ToggleUsagePanel,
        SaveFile,
        CancelTurn,
        CopySelection,
        OpenFind,
        OpenFindReplace,
        CloseFind,
        FindNext,
        FindPrevious,
        ToggleFindCaseSensitive,
        ToggleFindWholeWord,
        ToggleFindRegex,
        ReplaceAllMatches,
        BrowserBack,
        BrowserForward,
        BrowserReload,
        BrowserHardReload,
        BrowserStop,
        BrowserDevtools,
        FocusBrowserAddress,
        BrowserAddressCancel,
        WebviewCopy,
        WebviewCut,
        WebviewPaste,
        WebviewSelectAll
    ]
);

trait WakuApplicationExt {
    fn with_main_window_reopen(self) -> Self;
}

impl WakuApplicationExt for Application {
    fn with_main_window_reopen(self) -> Self {
        self.on_reopen(|cx| {
            if let Some(window) = cx.windows().into_iter().next() {
                window
                    .update(cx, |_, window, _| window.activate_window())
                    .ok();
            }
            cx.activate(true);
        });
        self
    }
}

fn main() {
    gpui_platform::application()
        .with_assets(crate::assets::Assets)
        .with_main_window_reopen()
        .run(|cx: &mut App| {
            crate::assets::register_fonts(cx).expect("failed to register bundled fonts");
            crate::input::init(cx);
            crate::ui::menu::init(cx);
            crate::app::init_composer_autocomplete(cx);
            crate::app::init_settings_keys(cx);
            crate::theme::init(cx);
            cx.set_reduce_motion(crate::platform::reduce_motion_enabled());

            // Sparkle only runs from a bundled release build (or when forced
            // via WAKU_FORCE_UPDATER=1); everywhere else the menu item is
            // omitted along with the updater itself.
            let updater = crate::updater::Updater::init();
            let updater_available = updater.is_some();
            cx.set_global(crate::updater::UpdaterState(updater));
            cx.on_action(|_: &CheckForUpdates, cx| {
                if let Some(updater) = &cx.global::<crate::updater::UpdaterState>().0 {
                    updater.check_for_updates();
                }
            });

            cx.bind_keys([
                KeyBinding::new("cmd-q", Quit, None),
                KeyBinding::new("cmd-w", CloseWindow, None),
                KeyBinding::new("cmd-n", NewSession, None),
                KeyBinding::new("cmd-,", OpenSettings, None),
                KeyBinding::new("cmd-b", ToggleSidebar, None),
                KeyBinding::new("cmd-shift-b", ToggleRightPanel, None),
                KeyBinding::new("cmd-alt-shift-f", ToggleFpsCounter, None),
                KeyBinding::new("cmd-[", NavigateBack, Some("Waku")),
                KeyBinding::new("cmd-]", NavigateForward, Some("Waku")),
                KeyBinding::new("cmd-l", FocusComposer, None),
                KeyBinding::new("cmd-/", ToggleModelPicker, None),
                KeyBinding::new("cmd-u", ToggleUsagePanel, None),
                KeyBinding::new("cmd-s", SaveFile, None),
                KeyBinding::new("escape", CancelTurn, Some("Waku")),
                KeyBinding::new("cmd-c", CopySelection, Some("Waku")),
                // Find and replace in the right panel's file editor, on the
                // conventional VS Code bindings. `cmd-g` cycles matches from
                // the editor without moving focus to the bar.
                KeyBinding::new("cmd-f", OpenFind, Some("Waku")),
                KeyBinding::new("cmd-alt-f", OpenFindReplace, Some("Waku")),
                KeyBinding::new("cmd-g", FindNext, Some("Waku")),
                KeyBinding::new("cmd-shift-g", FindPrevious, Some("Waku")),
                // Scoped to the editor pane: escape closes the bar there and
                // falls through to CancelTurn anywhere else.
                KeyBinding::new("escape", CloseFind, Some("FileEditorPane")),
                KeyBinding::new("cmd-alt-c", ToggleFindCaseSensitive, Some("FileEditorPane")),
                KeyBinding::new("cmd-alt-w", ToggleFindWholeWord, Some("FileEditorPane")),
                KeyBinding::new("cmd-alt-r", ToggleFindRegex, Some("FileEditorPane")),
                KeyBinding::new("shift-enter", FindPrevious, Some("FindBar")),
                KeyBinding::new("cmd-alt-enter", ReplaceAllMatches, Some("FindBar")),
                // Browser surface. Deeper than "Waku", so while focus is on the
                // page or its address bar the browser reads ⌘L/⌘R/⌘[/⌘]/Esc
                // the way every macOS browser does; the same keys elsewhere
                // keep their app meanings. The clipboard trio is rebound
                // because GPUI's window view claims key equivalents before
                // AppKit can walk the responder chain into the webview.
                KeyBinding::new("cmd-l", FocusBrowserAddress, Some("Browser")),
                KeyBinding::new("cmd-r", BrowserReload, Some("Browser")),
                KeyBinding::new("cmd-shift-r", BrowserHardReload, Some("Browser")),
                KeyBinding::new("cmd-[", BrowserBack, Some("Browser")),
                KeyBinding::new("cmd-]", BrowserForward, Some("Browser")),
                KeyBinding::new("escape", BrowserStop, Some("Browser")),
                KeyBinding::new("cmd-alt-i", BrowserDevtools, Some("Browser")),
                KeyBinding::new("cmd-c", WebviewCopy, Some("Browser")),
                KeyBinding::new("cmd-x", WebviewCut, Some("Browser")),
                KeyBinding::new("cmd-v", WebviewPaste, Some("Browser")),
                KeyBinding::new("cmd-a", WebviewSelectAll, Some("Browser")),
                KeyBinding::new("escape", BrowserAddressCancel, Some("BrowserAddress")),
            ]);

            cx.set_menus(vec![
                Menu {
                    name: APP_NAME.into(),
                    disabled: false,
                    items: {
                        let mut items = vec![
                            MenuItem::action("New Session", NewSession),
                            MenuItem::separator(),
                        ];
                        if updater_available {
                            items.push(MenuItem::action("Check for Updates…", CheckForUpdates));
                            items.push(MenuItem::separator());
                        }
                        items.extend([
                            MenuItem::action("Settings…", OpenSettings),
                            MenuItem::separator(),
                            MenuItem::action(format!("Quit {APP_NAME}"), Quit),
                        ]);
                        items
                    },
                },
                Menu {
                    name: "Edit".into(),
                    disabled: false,
                    items: vec![
                        MenuItem::action("Undo", input::Undo),
                        MenuItem::action("Redo", input::Redo),
                        MenuItem::separator(),
                        MenuItem::action("Cut", input::Cut),
                        MenuItem::action("Copy", input::Copy),
                        MenuItem::action("Paste", input::Paste),
                        MenuItem::action("Select All", input::SelectAll),
                    ],
                },
                Menu {
                    name: "View".into(),
                    disabled: false,
                    items: vec![
                        MenuItem::action("Toggle Sidebar", ToggleSidebar),
                        MenuItem::action("Toggle Right Panel", ToggleRightPanel),
                        MenuItem::action("Focus Composer", FocusComposer),
                        MenuItem::action("Toggle Model Picker", ToggleModelPicker),
                        MenuItem::action("Toggle Usage Panel", ToggleUsagePanel),
                    ],
                },
                Menu {
                    name: "Window".into(),
                    disabled: false,
                    items: vec![
                        MenuItem::action("Toggle FPS Counter", ToggleFpsCounter),
                        MenuItem::action("Close Window", CloseWindow),
                    ],
                },
            ]);

            cx.on_action(|_: &Quit, cx| cx.quit());

            let bounds = Bounds::centered(None, size(px(1380.0), px(880.0)), cx);

            let window = cx
                .open_window(
                    WindowOptions {
                        titlebar: Some(TitlebarOptions {
                            title: Some(APP_NAME.into()),
                            appears_transparent: true,
                            traffic_light_position: Some(point(px(16.0), px(17.0))),
                        }),
                        // Waku owns titlebar gestures so controls embedded in the header
                        // never inherit AppKit's implicit drag/double-click behavior.
                        is_movable: false,
                        window_background: WindowBackgroundAppearance::Blurred,
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        window_min_size: Some(size(px(980.0), px(680.0))),
                        ..Default::default()
                    },
                    |window, cx| {
                        crate::platform::configure_main_window_close_behavior(window, cx);
                        let waku = Waku::new(window, cx);
                        let composer_focus = waku.read(cx).composer_focus(cx);
                        window.focus(&composer_focus, cx);
                        waku
                    },
                )
                .expect("failed to open Waku window");

            window
                .update(cx, |_, window, cx| {
                    crate::platform::configure_sidebar_material(
                        window,
                        crate::theme::Theme::current(cx).is_dark,
                    );
                    cx.activate(true);
                })
                .ok();
        });
}
