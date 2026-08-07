//! In-app updates via Sparkle.
//!
//! `scripts/bundle.sh` embeds Sparkle.framework at Contents/Frameworks, and
//! this module loads it at runtime instead of linking it, so a bare `cargo
//! run` binary simply runs without an updater. Once loaded, a single
//! `SPUStandardUpdaterController` owns the whole update lifecycle: scheduled
//! background checks, downloads, EdDSA verification against `SUPublicEDKey`
//! in Info.plist, and the install-and-relaunch flow.
//!
//! Debug builds stay dormant so the dev watcher's app never offers to replace
//! itself with a production build; set `WAKU_FORCE_UPDATER=1` to exercise the
//! real update flow from a debug bundle anyway.

use gpui::Global;

/// App-wide handle to the updater, if this build can update itself.
pub struct UpdaterState(pub Option<Updater>);

impl Global for UpdaterState {}

#[cfg(target_os = "macos")]
pub struct Updater {
    controller: objc2::rc::Retained<objc2::runtime::AnyObject>,
}

#[cfg(target_os = "macos")]
impl Updater {
    /// Load Sparkle and start the shared updater controller. Returns `None`
    /// when this build cannot update itself: debug builds unless forced, and
    /// binaries running outside a bundle with an embedded framework.
    pub fn init() -> Option<Self> {
        use objc2::msg_send;
        use objc2::rc::Retained;
        use objc2::runtime::{AnyClass, AnyObject};

        let forced = std::env::var_os("WAKU_FORCE_UPDATER").is_some_and(|value| value == "1");
        if cfg!(debug_assertions) && !forced {
            return None;
        }

        let library = sparkle_library_path()?;
        let library_c =
            std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(library.as_os_str()))
                .ok()?;
        let handle = unsafe { libc::dlopen(library_c.as_ptr(), libc::RTLD_NOW) };
        if handle.is_null() {
            let reason = unsafe { libc::dlerror() };
            let reason = if reason.is_null() {
                "unknown dlopen failure".into()
            } else {
                unsafe { std::ffi::CStr::from_ptr(reason) }
                    .to_string_lossy()
                    .into_owned()
            };
            eprintln!("Waku updater: failed to load Sparkle: {reason}");
            return None;
        }

        let Some(class) = AnyClass::get(c"SPUStandardUpdaterController") else {
            eprintln!("Waku updater: Sparkle loaded but SPUStandardUpdaterController is missing");
            return None;
        };

        // Starting the updater arms Sparkle's scheduled checker and, on a
        // fresh install, its one-time "check automatically?" permission
        // prompt. Delegates are optional and unused.
        let controller = unsafe {
            let allocated: *mut AnyObject = msg_send![class, alloc];
            let initialized: *mut AnyObject = msg_send![
                allocated,
                initWithStartingUpdater: true,
                updaterDelegate: std::ptr::null_mut::<AnyObject>(),
                userDriverDelegate: std::ptr::null_mut::<AnyObject>()
            ];
            Retained::from_raw(initialized)?
        };

        let updater = Self { controller };

        // Starting only arms the *scheduled* checker, which stays quiet until
        // its interval has elapsed since the last check — so a plain relaunch
        // would never look for updates. Force one silent check per launch once
        // the user has consented to automatic checks. Sparkle wants this
        // immediately after the updater starts; calling it later interferes
        // with its scheduler.
        if updater.automatically_checks_for_updates() {
            let spu_updater = updater.spu_updater();
            let _: () = unsafe { msg_send![spu_updater, checkForUpdatesInBackground] };
        }

        Some(updater)
    }

    /// Run the user-facing update check with Sparkle's own progress UI.
    /// Ignored while another update session is already in flight.
    pub fn check_for_updates(&self) {
        use objc2::msg_send;
        use objc2::runtime::AnyObject;

        let spu_updater = self.spu_updater();
        let can_check: bool = unsafe { msg_send![spu_updater, canCheckForUpdates] };
        if !can_check {
            return;
        }
        let _: () = unsafe {
            msg_send![&*self.controller, checkForUpdates: std::ptr::null_mut::<AnyObject>()]
        };
    }

    /// Whether Sparkle checks for updates on its own schedule. Sparkle owns
    /// the persisted value in this app's user defaults.
    pub fn automatically_checks_for_updates(&self) -> bool {
        use objc2::msg_send;

        let spu_updater = self.spu_updater();
        unsafe { msg_send![spu_updater, automaticallyChecksForUpdates] }
    }

    pub fn set_automatically_checks_for_updates(&self, enabled: bool) {
        use objc2::msg_send;

        let spu_updater = self.spu_updater();
        let _: () = unsafe { msg_send![spu_updater, setAutomaticallyChecksForUpdates: enabled] };
    }

    /// The controller's `SPUUpdater`, which carries the checking state and the
    /// persisted settings.
    fn spu_updater(&self) -> *mut objc2::runtime::AnyObject {
        use objc2::msg_send;

        unsafe { msg_send![&*self.controller, updater] }
    }
}

/// The embedded framework's dylib next to the running executable
/// (Contents/MacOS/Waku → Contents/Frameworks/Sparkle.framework/Sparkle).
#[cfg(target_os = "macos")]
fn sparkle_library_path() -> Option<std::path::PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let contents = executable.parent()?.parent()?;
    let library = contents.join("Frameworks/Sparkle.framework/Sparkle");
    library.exists().then_some(library)
}

/// Non-macOS builds have no updater yet. This stub is the seam where a
/// platform implementation slots in (WinSparkle consumes the same appcast
/// format on Windows); callers already treat `None` as "no updater".
#[cfg(not(target_os = "macos"))]
pub struct Updater;

#[cfg(not(target_os = "macos"))]
impl Updater {
    pub fn init() -> Option<Self> {
        None
    }

    pub fn check_for_updates(&self) {}

    pub fn automatically_checks_for_updates(&self) -> bool {
        false
    }

    pub fn set_automatically_checks_for_updates(&self, _enabled: bool) {}
}
