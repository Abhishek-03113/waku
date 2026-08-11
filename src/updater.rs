//! In-app updates via Sparkle.
//!
//! `scripts/bundle.sh` embeds Sparkle.framework at Contents/Frameworks, and
//! this module loads it at runtime instead of linking it, so a bare `cargo
//! run` binary simply runs without an updater. Sparkle still owns update
//! discovery, download, signature verification, installation, and relaunch;
//! Waku supplies a custom user driver so update availability and progress can
//! live in the sidebar instead of a separate updater window.
//!
//! Debug builds stay dormant so the dev watcher's app never offers to replace
//! itself with a production build; set `WAKU_FORCE_UPDATER=1` to exercise the
//! real update flow from a debug bundle anyway.

use gpui::Global;

/// App-wide handle to the updater, if this build can update itself.
pub struct UpdaterState(pub Option<Updater>);

impl Global for UpdaterState {}

/// The compact state rendered by Waku. Update details remain owned by
/// Sparkle and never enter a frame path.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UpdateStatus {
    #[default]
    Idle,
    Checking,
    Available,
    Updating,
}

#[derive(Clone, Debug)]
pub enum UpdaterEvent {
    StatusChanged(UpdateStatus),
    UpToDate,
    Failed(String),
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use block2::{DynBlock, RcBlock};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol};
    use objc2::{
        DefinedClass, MainThreadMarker, MainThreadOnly, define_class, extern_protocol, msg_send,
    };
    use objc2_foundation::NSString;

    use super::{UpdateStatus, UpdaterEvent};

    const USER_UPDATE_CHOICE_INSTALL: isize = 1;

    extern_protocol!(
        /// Dynamically loaded from the embedded Sparkle framework.
        unsafe trait SPUUserDriver: NSObjectProtocol {}
    );

    struct UserDriverIvars {
        /// Sparkle's standard driver is retained solely for its one-time
        /// automatic-check permission prompt. Update presentation itself is
        /// entirely handled by Waku.
        permission_driver: Retained<AnyObject>,
        install_reply: RefCell<Option<RcBlock<dyn Fn(isize)>>>,
        status: Rc<Cell<UpdateStatus>>,
        events: smol::channel::Sender<UpdaterEvent>,
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "WakuSparkleUserDriver"]
        #[thread_kind = MainThreadOnly]
        #[ivars = UserDriverIvars]
        struct UserDriver;

        impl UserDriver {
            #[unsafe(method(showUpdatePermissionRequest:reply:))]
            fn show_update_permission_request(
                &self,
                request: &AnyObject,
                reply: &DynBlock<dyn Fn(*mut AnyObject)>,
            ) {
                let _: () = unsafe {
                    msg_send![
                        &*self.ivars().permission_driver,
                        showUpdatePermissionRequest: request,
                        reply: reply
                    ]
                };
            }

            #[unsafe(method(showUserInitiatedUpdateCheckWithCancellation:))]
            fn show_user_initiated_update_check(&self, _cancellation: &DynBlock<dyn Fn()>) {
                self.set_status(UpdateStatus::Checking);
            }

            #[unsafe(method(showUpdateFoundWithAppcastItem:state:reply:))]
            fn show_update_found(
                &self,
                _appcast_item: &AnyObject,
                _state: &AnyObject,
                reply: &DynBlock<dyn Fn(isize)>,
            ) {
                self.ivars().install_reply.replace(Some(reply.copy()));
                self.set_status(UpdateStatus::Available);
            }

            #[unsafe(method(showUpdateReleaseNotesWithDownloadData:))]
            fn show_update_release_notes(&self, _download_data: &AnyObject) {}

            #[unsafe(method(showUpdateReleaseNotesFailedToDownloadWithError:))]
            fn show_update_release_notes_failed(&self, _error: &AnyObject) {}

            #[unsafe(method(showUpdateNotFoundWithError:acknowledgement:))]
            fn show_update_not_found(
                &self,
                _error: &AnyObject,
                acknowledgement: &DynBlock<dyn Fn()>,
            ) {
                self.clear_update();
                self.send(UpdaterEvent::UpToDate);
                acknowledgement.call(());
            }

            #[unsafe(method(showUpdaterError:acknowledgement:))]
            fn show_updater_error(
                &self,
                error: &AnyObject,
                acknowledgement: &DynBlock<dyn Fn()>,
            ) {
                self.clear_update();
                self.send(UpdaterEvent::Failed(error_description(error)));
                acknowledgement.call(());
            }

            #[unsafe(method(showDownloadInitiatedWithCancellation:))]
            fn show_download_initiated(&self, _cancellation: &DynBlock<dyn Fn()>) {
                self.set_status(UpdateStatus::Updating);
            }

            #[unsafe(method(showDownloadDidReceiveExpectedContentLength:))]
            fn show_expected_content_length(&self, _expected_content_length: u64) {}

            #[unsafe(method(showDownloadDidReceiveDataOfLength:))]
            fn show_downloaded_data(&self, _length: u64) {}

            #[unsafe(method(showDownloadDidStartExtractingUpdate))]
            fn show_extracting_update(&self) {
                self.set_status(UpdateStatus::Updating);
            }

            #[unsafe(method(showExtractionReceivedProgress:))]
            fn show_extraction_progress(&self, _progress: f64) {}

            #[unsafe(method(showReadyToInstallAndRelaunch:))]
            fn show_ready_to_install(&self, reply: &DynBlock<dyn Fn(isize)>) {
                self.set_status(UpdateStatus::Updating);
                reply.call((USER_UPDATE_CHOICE_INSTALL,));
            }

            #[unsafe(method(showInstallingUpdateWithApplicationTerminated:retryTerminatingApplication:))]
            fn show_installing_update(
                &self,
                _application_terminated: bool,
                _retry_terminating_application: &DynBlock<dyn Fn()>,
            ) {
                self.set_status(UpdateStatus::Updating);
            }

            #[unsafe(method(showUpdateInstalledAndRelaunched:acknowledgement:))]
            fn show_update_installed(
                &self,
                _relaunched: bool,
                acknowledgement: &DynBlock<dyn Fn()>,
            ) {
                acknowledgement.call(());
            }

            #[unsafe(method(dismissUpdateInstallation))]
            fn dismiss_update_installation(&self) {
                self.clear_update();
            }

            #[unsafe(method(showUpdateInFocus))]
            fn show_update_in_focus(&self) {}
        }

        unsafe impl NSObjectProtocol for UserDriver {}
        unsafe impl SPUUserDriver for UserDriver {}
    );

    impl UserDriver {
        fn new(
            mtm: MainThreadMarker,
            permission_driver: Retained<AnyObject>,
            status: Rc<Cell<UpdateStatus>>,
            events: smol::channel::Sender<UpdaterEvent>,
        ) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(UserDriverIvars {
                permission_driver,
                install_reply: RefCell::new(None),
                status,
                events,
            });
            unsafe { msg_send![super(this), init] }
        }

        fn send(&self, event: UpdaterEvent) {
            let _ = self.ivars().events.try_send(event);
        }

        fn set_status(&self, status: UpdateStatus) {
            if self.ivars().status.replace(status) != status {
                self.send(UpdaterEvent::StatusChanged(status));
            }
        }

        fn clear_update(&self) {
            self.ivars().install_reply.borrow_mut().take();
            self.set_status(UpdateStatus::Idle);
        }

        fn install_available_update(&self) -> bool {
            let Some(reply) = self.ivars().install_reply.borrow_mut().take() else {
                return false;
            };
            self.set_status(UpdateStatus::Updating);
            reply.call((USER_UPDATE_CHOICE_INSTALL,));
            true
        }
    }

    pub struct Updater {
        updater: Option<Retained<AnyObject>>,
        user_driver: Option<Retained<UserDriver>>,
        status: Rc<Cell<UpdateStatus>>,
        events: smol::channel::Receiver<UpdaterEvent>,
        preview_events: Option<smol::channel::Sender<UpdaterEvent>>,
    }

    impl Updater {
        /// Load Sparkle and start its updater. Returns `None` when this build
        /// cannot update itself: debug builds unless forced, and binaries
        /// running outside a bundle with an embedded framework.
        pub fn init() -> Option<Self> {
            if cfg!(debug_assertions)
                && std::env::var_os("WAKU_PREVIEW_UPDATE").is_some_and(|value| value == "1")
            {
                return Some(Self::preview());
            }

            let forced = std::env::var_os("WAKU_FORCE_UPDATER").is_some_and(|value| value == "1");
            if cfg!(debug_assertions) && !forced {
                return None;
            }

            let mtm = MainThreadMarker::new()?;
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

            let bundle_class = AnyClass::get(c"NSBundle")?;
            let updater_class = AnyClass::get(c"SPUUpdater")?;
            let permission_driver_class = AnyClass::get(c"SPUStandardUserDriver")?;
            let main_bundle: *mut AnyObject = unsafe { msg_send![bundle_class, mainBundle] };
            if main_bundle.is_null() {
                return None;
            }

            let permission_driver = unsafe {
                let allocated: *mut AnyObject = msg_send![permission_driver_class, alloc];
                let initialized: *mut AnyObject = msg_send![
                    allocated,
                    initWithHostBundle: main_bundle,
                    delegate: std::ptr::null_mut::<AnyObject>()
                ];
                Retained::from_raw(initialized)?
            };

            let status = Rc::new(Cell::new(UpdateStatus::Idle));
            let (event_tx, events) = smol::channel::unbounded();
            let user_driver = UserDriver::new(mtm, permission_driver, status.clone(), event_tx);
            let updater = unsafe {
                let allocated: *mut AnyObject = msg_send![updater_class, alloc];
                let initialized: *mut AnyObject = msg_send![
                    allocated,
                    initWithHostBundle: main_bundle,
                    applicationBundle: main_bundle,
                    userDriver: &*user_driver,
                    delegate: std::ptr::null_mut::<AnyObject>()
                ];
                Retained::from_raw(initialized)?
            };

            let started: bool = unsafe {
                msg_send![
                    &*updater,
                    startUpdater: std::ptr::null_mut::<*mut AnyObject>()
                ]
            };
            if !started {
                eprintln!("Waku updater: Sparkle rejected its updater configuration");
                return None;
            }

            let updater = Self {
                updater: Some(updater),
                user_driver: Some(user_driver),
                status,
                events,
                preview_events: None,
            };

            // Starting only arms the scheduled checker, which stays quiet
            // until its interval has elapsed since the last check. Force one
            // silent check per launch once the user has consented.
            if updater.automatically_checks_for_updates() {
                let sparkle = updater
                    .updater
                    .as_ref()
                    .expect("Sparkle updater initialized");
                let _: () = unsafe { msg_send![&**sparkle, checkForUpdatesInBackground] };
            }

            Some(updater)
        }

        /// Run a user-initiated check through Waku's sidebar presentation.
        pub fn check_for_updates(&self) {
            if let Some(updater) = &self.updater {
                let can_check: bool = unsafe { msg_send![&**updater, canCheckForUpdates] };
                if can_check {
                    let _: () = unsafe { msg_send![&**updater, checkForUpdates] };
                }
            } else {
                self.set_preview_status(UpdateStatus::Available);
            }
        }

        pub fn install_available_update(&self) -> bool {
            if let Some(user_driver) = &self.user_driver {
                user_driver.install_available_update()
            } else if self.status.get() == UpdateStatus::Available {
                self.set_preview_status(UpdateStatus::Updating);
                true
            } else {
                false
            }
        }

        pub fn status(&self) -> UpdateStatus {
            self.status.get()
        }

        pub fn events(&self) -> smol::channel::Receiver<UpdaterEvent> {
            self.events.clone()
        }

        /// Whether Sparkle checks for updates on its own schedule. Sparkle
        /// owns the persisted value in this app's user defaults.
        pub fn automatically_checks_for_updates(&self) -> bool {
            self.updater.as_ref().is_some_and(|updater| unsafe {
                msg_send![&**updater, automaticallyChecksForUpdates]
            })
        }

        pub fn set_automatically_checks_for_updates(&self, enabled: bool) {
            if let Some(updater) = &self.updater {
                let _: () =
                    unsafe { msg_send![&**updater, setAutomaticallyChecksForUpdates: enabled] };
            }
        }

        fn preview() -> Self {
            let status = Rc::new(Cell::new(UpdateStatus::Available));
            let (preview_events, events) = smol::channel::unbounded();
            Self {
                updater: None,
                user_driver: None,
                status,
                events,
                preview_events: Some(preview_events),
            }
        }

        fn set_preview_status(&self, status: UpdateStatus) {
            if self.status.replace(status) != status
                && let Some(events) = &self.preview_events
            {
                let _ = events.try_send(UpdaterEvent::StatusChanged(status));
            }
        }
    }

    fn error_description(error: &AnyObject) -> String {
        let description: *mut NSString = unsafe { msg_send![error, localizedDescription] };
        unsafe { description.as_ref() }
            .map(ToString::to_string)
            .unwrap_or_else(|| "Unknown updater error".to_owned())
    }

    /// The embedded framework's dylib next to the running executable
    /// (Contents/MacOS/Waku → Contents/Frameworks/Sparkle.framework/Sparkle).
    fn sparkle_library_path() -> Option<std::path::PathBuf> {
        let executable = std::env::current_exe().ok()?;
        let contents = executable.parent()?.parent()?;
        let library = contents.join("Frameworks/Sparkle.framework/Sparkle");
        library.exists().then_some(library)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn preview_update_switches_from_available_to_spinner() {
            let updater = Updater::preview();
            assert_eq!(updater.status(), UpdateStatus::Available);
            assert!(updater.install_available_update());
            assert_eq!(updater.status(), UpdateStatus::Updating);
            assert!(matches!(
                updater.events().try_recv(),
                Ok(UpdaterEvent::StatusChanged(UpdateStatus::Updating))
            ));
        }
    }
}

#[cfg(target_os = "macos")]
pub use macos::Updater;

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

    pub fn install_available_update(&self) -> bool {
        false
    }

    pub fn status(&self) -> UpdateStatus {
        UpdateStatus::Idle
    }

    pub fn events(&self) -> smol::channel::Receiver<UpdaterEvent> {
        let (_tx, rx) = smol::channel::unbounded();
        rx
    }

    pub fn automatically_checks_for_updates(&self) -> bool {
        false
    }

    pub fn set_automatically_checks_for_updates(&self, _enabled: bool) {}
}
