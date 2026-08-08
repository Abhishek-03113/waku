use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};
use crossbeam_channel::{Receiver, Sender, unbounded};
use gpui::{
    Animation, AnimationExt, AnyElement, App, ClipboardItem, Context, Div, Entity, ExternalPaths,
    FocusHandle, Focusable, FontWeight, Hsla, IntoElement, KeyDownEvent, ListAlignment, ListOffset,
    ListState, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, NavigationDirection,
    ObjectFit, PathPromptOptions, Pixels, Render, ScrollHandle, SharedString, Stateful,
    StyleRefinement, WeakEntity, Window, canvas, div, ease_out_quint, fill, img, linear_color_stop,
    linear_gradient, list, point, prelude::*, pulsating_between, px, rgb,
};
use uuid::Uuid;

use crate::checkpoint;
use crate::composer_complete::{FileEntry, SlashCommand};
use crate::computer_use::{
    ComputerPermissions, ComputerUsePhase, ComputerUseState, PendingComputerApproval,
};
use crate::driver::{self, DriverHandle, DriverStartOptions, SessionOptions};
use crate::git_branch::BranchSnapshot;
use crate::input::{ComposerEvent, ComposerInput};
use crate::md;
use crate::model::{
    ActivityItem, AgentSession, Checkpoint, CheckpointStatus, ContextUsage, DriverEvent,
    FavoriteModel, InteractionMode, Message, MessageRole, PendingPermission, Project, ProviderKind,
    ProviderModel, ProviderProbe, ProviderResumeCursor, QueuedMessage, ReasoningBlock, RuntimeMode,
    SessionStatus, SessionWorkspace, TranscriptBlock, TranscriptBlockContent, TurnStatus,
    compact_path, unix_time, unix_time_millis,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::md::render::{
    Ctx as MarkdownCtx, MarkdownView, Metrics as MarkdownMetrics, Palette as MarkdownPalette,
    TranscriptSelection,
};
use crate::ui::menu::{
    ConfirmEntry, ContextMenuHandle, DismissMenu, MenuAlign, MenuItem, SelectNextEntry,
    SelectNextTab, SelectPreviousEntry, SelectPreviousTab, context_menu, dropdown_menu, popover,
};
use crate::ui::scrollbar::{self, ScrollbarState};
use crate::ui::tooltip::Tooltip;

use crate::browser::BrowserView;
use crate::persistence::{
    DEFAULT_RIGHT_PANEL_WIDTH, DEFAULT_SIDEBAR_WIDTH, PersistedState, StateStore,
};
use crate::query::{Query, QueryCache};
use crate::terminal::TerminalView;
use crate::theme::{Theme, ThemePreference};
use crate::ui::text_field::TextField;
use crate::ui::{
    MenuChip, ProjectNameSelector, activity_icon, activity_noun, icon, icon_button, provider_color,
    provider_icon, status_color,
};
use crate::{
    CancelTurn, CloseFind, CloseWindow, CopySelection, FindNext, FindPrevious, FocusComposer,
    NavigateBack, NavigateForward, NewSession, OpenFind, OpenFindReplace, OpenSettings,
    ReplaceAllMatches, SaveFile, ToggleFindCaseSensitive, ToggleFindRegex, ToggleFindWholeWord,
    ToggleFpsCounter, ToggleModelPicker, ToggleRightPanel, ToggleSidebar, ToggleUsagePanel,
};

const TRAFFIC_LIGHT_CLEARANCE: f32 = 86.0;
const CONTENT_MAX_WIDTH: f32 = 720.0;
/// Menu-registry id of the composer's model picker, shared by its render site
/// and the `cmd-/` toggle action.
const MODEL_PICKER_MENU_ID: &str = "provider-model-picker";
const BRANCH_PICKER_MENU_ID: &str = "workspace-branch-picker";
const BRANCH_PICKER_ROW_HEIGHT: f32 = 26.0;
const SIDEBAR_MIN_WIDTH: f32 = 180.0;
const SIDEBAR_MAX_WIDTH: f32 = 420.0;
const RIGHT_PANEL_MIN_WIDTH: f32 = 280.0;
const RIGHT_PANEL_MAX_WIDTH: f32 = 1000.0;
const DEFAULT_FILE_TREE_WIDTH: f32 = 184.0;
const FILE_TREE_MIN_WIDTH: f32 = 140.0;
const FILE_TREE_MAX_WIDTH: f32 = 360.0;
const FILE_EDITOR_MIN_WIDTH: f32 = 140.0;
const FILE_EDITOR_INITIAL_WIDTH: f32 = 500.0;
const MAIN_PANEL_MIN_WIDTH: f32 = 360.0;
const FOLLOWUP_TURN_TOP_GAP: f32 = 48.0;
const NAVIGATION_RAIL_WIDTH: f32 = 44.0;
const NAVIGATION_RAIL_LEFT: f32 = 16.0;
const NAVIGATION_RAIL_CONTENT_GAP: f32 = 16.0;
const NAVIGATION_RAIL_VIEWPORT_HEIGHT_RATIO: f32 = 0.80;
const NAVIGATION_RAIL_TICK_WIDTH: f32 = 32.0;
const NAVIGATION_RAIL_TICK_HEIGHT: f32 = 2.0;
const NAVIGATION_RAIL_TICK_GAP: f32 = 10.0;
const NAVIGATION_RAIL_INACTIVE_OPACITY: f32 = 0.45;
const NAVIGATION_RAIL_TURN_HEIGHT: f32 = NAVIGATION_RAIL_TICK_HEIGHT + NAVIGATION_RAIL_TICK_GAP;
const NAVIGATION_RAIL_ANIMATION_DURATION: Duration = Duration::from_millis(300);
const STREAM_FRAME_INTERVAL: Duration = Duration::from_millis(24);
/// How long a session may sit untouched before its provider process is released.
/// Codex and Pi stay resident between turns, so without this an afternoon of
/// abandoned tasks is an afternoon of idle agent processes.
const IDLE_SESSION_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const IDLE_SESSION_SWEEP_INTERVAL: Duration = Duration::from_secs(5 * 60);
const STREAM_SAVE_INTERVAL: Duration = Duration::from_secs(1);
/// Zed keeps status toasts on screen for ten seconds, pausing the countdown
/// while the pointer is over the toast so a long message remains readable.
const DEFAULT_TOAST_DURATION: Duration = Duration::from_secs(5);
const MINIMUM_TOAST_RESUME_DURATION: Duration = Duration::from_millis(800);
const TOAST_ANIMATION_DURATION: Duration = Duration::from_millis(150);
/// Source bytes of parsed messages kept across session switches.
///
/// Measured at ~17x expansion into parsed structures, plus flattened text and
/// shaped runs on top, so this is bounded by source size rather than entry
/// count — one long message costs far more than several short ones. 512 KB
/// holds several sessions' transcripts for a few MB of structures.
const MAX_CACHED_MESSAGE_SOURCE_BYTES: usize = 512 * 1024;
/// Projects whose workspace lookups are remembered — branch, diff listing,
/// working tree. A window rarely has more than a handful open, and the diff
/// and tree caches are invalidated on every refresh, so they hold one entry in
/// practice. 8 is generous and caps the tree cache, the only large one, at a
/// few hundred KB.
const MAX_CACHED_WORKSPACES: usize = 8;
const STREAM_CATCH_UP_FRAMES: usize = 18;
const STREAM_MIN_GRAPHEMES_PER_FRAME: usize = 12;
const STREAM_MAX_GRAPHEMES_PER_FRAME: usize = 256;
const STREAM_REMEASURE_TAIL_ROWS: usize = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StreamPhase {
    Text,
    Reasoning,
    Activity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StreamDeltaKind {
    Text,
    Reasoning,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModelPickerTab {
    Favorites,
    Provider(ProviderKind),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum BranchPickerMode {
    #[default]
    Browse,
    Create,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum BranchPickerAction {
    Checkout(String),
    Create,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SettingsPage {
    General,
    Providers,
    Usage,
    ComputerUse,
    Appearance,
}

/// Which presentation the Usage page shows: the daily dashboard, the monthly
/// statement, or the per-project ranking.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UsageViewMode {
    Daily,
    Monthly,
    Projects,
}

/// Which unit the Usage page's headline and chart read in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UsageMetric {
    Cost,
    Tokens,
}

/// Which table the Usage page's breakdown section shows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UsageBreakdown {
    Model,
    Day,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PanelResizeTarget {
    Sidebar,
    RightPanel,
    FileTree,
}

#[derive(Clone, Copy, Debug)]
struct PanelResizeDrag {
    target: PanelResizeTarget,
    start_mouse_x: f32,
    start_width: f32,
}

#[derive(Debug)]
struct ToastState {
    message: String,
    id: u64,
    timer_generation: u64,
    duration_remaining: Duration,
    timer_started: Option<Instant>,
    hovered: bool,
}

fn paused_toast_duration(remaining: Duration, elapsed: Duration) -> Duration {
    remaining
        .saturating_sub(elapsed)
        .max(MINIMUM_TOAST_RESUME_DURATION)
}

/// A file dropped onto the composer, staged as a chip until the next
/// submission carries it as an `@` mention.
#[derive(Clone, Debug)]
struct ComposerAttachment {
    /// Absolute path as dropped — the thumbnail reads this.
    path: PathBuf,
    /// What the submission sends: relative to the project root when the file
    /// is inside it, absolute otherwise, directories with a trailing slash.
    mention: String,
    /// Basename drawn on the chip.
    name: SharedString,
    is_dir: bool,
    /// Whether the chip shows a thumbnail. Decided by extension at drop time
    /// so render never touches the filesystem.
    is_image: bool,
}

/// Whether an untouched session's provider process may be released.
///
/// A session mid-turn is not idle however long it has been quiet: a slow tool
/// call, or an approval waiting on the user, must not have its agent pulled out
/// from under it.
fn session_is_reapable(session: Option<&AgentSession>, idle_for: Duration) -> bool {
    idle_for >= IDLE_SESSION_TIMEOUT
        && session.is_none_or(|session| {
            session.active_turn_id().is_none()
                && matches!(session.status, SessionStatus::Idle | SessionStatus::Failed)
        })
}

fn sanitize_panel_width(width: f32, default: f32, min: f32, max: f32) -> f32 {
    if width.is_finite() {
        width.clamp(min, max)
    } else {
        default
    }
}

fn fitted_file_tree_width(panel_width: f32, file_tree_width: f32) -> f32 {
    let maximum = FILE_TREE_MAX_WIDTH
        .min(panel_width - FILE_EDITOR_MIN_WIDTH)
        .max(FILE_TREE_MIN_WIDTH);
    sanitize_panel_width(
        file_tree_width,
        DEFAULT_FILE_TREE_WIDTH.clamp(FILE_TREE_MIN_WIDTH, maximum),
        FILE_TREE_MIN_WIDTH,
        maximum,
    )
}

fn widened_panel_width_for_file_editor(panel_width: f32, file_tree_width: f32) -> f32 {
    let panel_width = sanitize_panel_width(
        panel_width,
        DEFAULT_RIGHT_PANEL_WIDTH,
        RIGHT_PANEL_MIN_WIDTH,
        RIGHT_PANEL_MAX_WIDTH,
    );
    let file_tree_width = sanitize_panel_width(
        file_tree_width,
        DEFAULT_FILE_TREE_WIDTH,
        FILE_TREE_MIN_WIDTH,
        FILE_TREE_MAX_WIDTH,
    );
    panel_width
        .max(file_tree_width + FILE_EDITOR_INITIAL_WIDTH)
        .min(RIGHT_PANEL_MAX_WIDTH)
}

fn fitted_panel_widths(
    viewport_width: f32,
    sidebar_visible: bool,
    right_panel_visible: bool,
    sidebar_width: f32,
    right_panel_width: f32,
) -> (f32, f32) {
    let sidebar_min = if sidebar_visible {
        SIDEBAR_MIN_WIDTH
    } else {
        0.0
    };
    let right_panel_min = if right_panel_visible {
        RIGHT_PANEL_MIN_WIDTH
    } else {
        0.0
    };
    let mut sidebar = if sidebar_visible {
        sanitize_panel_width(
            sidebar_width,
            DEFAULT_SIDEBAR_WIDTH,
            SIDEBAR_MIN_WIDTH,
            SIDEBAR_MAX_WIDTH,
        )
    } else {
        0.0
    };
    let mut right_panel = if right_panel_visible {
        sanitize_panel_width(
            right_panel_width,
            DEFAULT_RIGHT_PANEL_WIDTH,
            RIGHT_PANEL_MIN_WIDTH,
            RIGHT_PANEL_MAX_WIDTH,
        )
    } else {
        0.0
    };

    let available = (viewport_width - MAIN_PANEL_MIN_WIDTH).max(0.0);
    let mut overflow = (sidebar + right_panel - available).max(0.0);
    let right_reduction = overflow.min((right_panel - right_panel_min).max(0.0));
    right_panel -= right_reduction;
    overflow -= right_reduction;
    let sidebar_reduction = overflow.min((sidebar - sidebar_min).max(0.0));
    sidebar -= sidebar_reduction;
    overflow -= sidebar_reduction;

    // The configured minimum window easily fits both panel minima. This final
    // fallback only protects layout if the host temporarily reports a smaller
    // viewport during a resize or display transition.
    if overflow > 0.0 {
        let right_reduction = overflow.min(right_panel);
        right_panel -= right_reduction;
        overflow -= right_reduction;
        sidebar = (sidebar - overflow).max(0.0);
    }

    (sidebar, right_panel)
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RightPanelSurface {
    Browser(Uuid),
    Terminal(Uuid),
    Files,
    Diff,
    File(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RightPanelDiffFile {
    path: String,
    additions: u64,
    deletions: u64,
}

/// A turn whose checkpoint still has to be captured.
struct PendingCheckpointCapture {
    session_id: Uuid,
    turn_count: usize,
    project_path: PathBuf,
}

/// Sessions between accepting a submission and handing it to a provider.
///
/// Worktree creation and the pre-turn checkpoint both run off the UI thread,
/// but neither operation has a safe interrupt contract. Keeping this separate
/// from [`SessionStatus`] lets the composer distinguish that non-cancellable
/// preparation window from a connecting provider that can already be stopped.
struct PreparedSubmission {
    workspace: SessionWorkspace,
    checkpoint_warning: Option<String>,
    /// `None` reuses an already-live runtime. `Some` contains the result of a
    /// provider process start performed on the background executor.
    driver: Option<anyhow::Result<PreparedDriver>>,
}

/// Everything needed to start a provider process, captured while the session
/// is still on the UI thread. `cwd` is replaced with the materialized
/// worktree path by the background preparation task.
struct DriverStartRequest {
    provider: ProviderKind,
    options: DriverStartOptions,
}

/// A provider process that has started off-thread but is not installed into
/// Waku's runtime map yet. Its event receiver safely buffers early events.
struct PreparedDriver {
    handle: DriverHandle,
    events: Receiver<DriverEvent>,
}

struct RightPanelFileEditor {
    state: Entity<ComposerInput>,
    disk_content: String,
    writable: bool,
    dirty: bool,
    /// A read is in flight on the background executor. Set from the moment the
    /// editor is created, because `render` may not touch the filesystem: until
    /// the first read lands the editor is empty and locked, and that means
    /// "not read yet", never "empty file".
    reading: bool,
    /// Bumped whenever the editor's idea of the file changes, so a read that
    /// started earlier cannot apply over a newer truth — a save in particular,
    /// which makes any read already in flight describe the pre-save file.
    read_epoch: u64,
}

struct RightPanelSessionState {
    visible: bool,
    surfaces: Vec<RightPanelSurface>,
    active_surface: Option<usize>,
    tabs_scroll_handle: ScrollHandle,
    pending_tab_reveal: Option<usize>,
    expanded_paths: HashSet<PathBuf>,
    files_selected_path: Option<String>,
    file_tree_width: f32,
    file_editors: HashMap<String, RightPanelFileEditor>,
    diff_files: Vec<RightPanelDiffFile>,
}

impl RightPanelSessionState {
    fn empty(visible: bool) -> Self {
        Self {
            visible,
            surfaces: Vec::new(),
            active_surface: None,
            tabs_scroll_handle: ScrollHandle::new(),
            pending_tab_reveal: None,
            expanded_paths: HashSet::new(),
            files_selected_path: None,
            file_tree_width: DEFAULT_FILE_TREE_WIDTH,
            file_editors: HashMap::new(),
            diff_files: Vec::new(),
        }
    }

    fn take_or_closed(states: &mut HashMap<Uuid, Self>, session_id: Uuid) -> Self {
        states
            .remove(&session_id)
            .unwrap_or_else(|| Self::empty(false))
    }
}

/// One choice in the model-traits menu: a label plus a badge marking the
/// provider's own default, so the current selection and the default read apart.
fn traits_choice(theme: Theme, label: String, is_default: bool) -> MenuItem {
    MenuItem::custom(move |_, _| {
        div()
            .w(px(190.0))
            .py(px(2.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_color(theme.text_secondary)
                    .child(label.clone()),
            )
            .when(is_default, |element| {
                element.child(
                    div()
                        .h(px(16.0))
                        .px(px(5.0))
                        .flex_none()
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(theme.border_strong)
                        .bg(theme.overlay)
                        .flex()
                        .items_center()
                        .text_size(px(9.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text_tertiary)
                        .child("Default"),
                )
            })
            .into_any_element()
    })
}

#[derive(Clone, Copy, Debug)]
struct UserMessageAction {
    session_id: Uuid,
    turn_count: usize,
}

#[derive(Clone, Copy, Debug)]
struct AssistantMessageAction {
    session_id: Uuid,
    turn_count: usize,
}

#[derive(Clone)]
struct MessageEdit {
    session_id: Uuid,
    turn_count: usize,
    input: Entity<ComposerInput>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TranscriptAnchor {
    session_id: Uuid,
    turn_id: Uuid,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct NavigationRailVisualState {
    active_turn: Option<Uuid>,
    emphasized_turn: Option<Uuid>,
}

struct SessionRuntime {
    driver: DriverHandle,
    events: Receiver<DriverEvent>,
    pending_events: VecDeque<DriverEvent>,
    stream_phase: Option<StreamPhase>,
    stream_remeasure_pending: bool,
    pending_permission: Option<PendingPermission>,
    pending_computer_approval: Option<PendingComputerApproval>,
    /// Back-to-front stack of window previews captured during the active turn.
    computer_use_previews: Vec<ComputerUseState>,
    computer_session_grants: HashSet<String>,
    last_driver_error: Option<String>,
    /// When this session last sent or received anything, for idle reaping.
    last_active_at: Instant,
}

#[derive(Debug, Default)]
struct SessionNavigation {
    back: Vec<Uuid>,
    forward: Vec<Uuid>,
}

impl SessionNavigation {
    fn visit(&mut self, current: Option<Uuid>, next: Uuid) {
        if let Some(current) = current.filter(|current| *current != next) {
            self.back.push(current);
            self.forward.clear();
        }
    }

    fn go_back(&mut self, current: Uuid) -> Option<Uuid> {
        let target = self.back.pop()?;
        self.forward.push(current);
        Some(target)
    }

    fn go_forward(&mut self, current: Uuid) -> Option<Uuid> {
        let target = self.forward.pop()?;
        self.back.push(current);
        Some(target)
    }

    fn remove(&mut self, session_id: Uuid) {
        self.back.retain(|entry| *entry != session_id);
        self.forward.retain(|entry| *entry != session_id);
    }
}

pub struct Waku {
    state: PersistedState,
    store: StateStore,
    composer: Entity<ComposerInput>,
    model_search: Entity<ComposerInput>,
    settings_search: Entity<ComposerInput>,
    settings_focus: FocusHandle,
    onboarding_add_project_focus: FocusHandle,
    onboarding_projectless_focus: FocusHandle,
    /// Mirror of Sparkle's persisted automatic-check setting. Refreshed when
    /// settings opens and on toggle, so frames never read user defaults —
    /// that lookup can reach cfprefsd.
    automatic_updates_enabled: bool,
    probes: Vec<ProviderProbe>,
    provider_probe_tx: Sender<ProviderProbe>,
    provider_probe_events: Receiver<ProviderProbe>,
    provider_model_discoveries: HashSet<ProviderKind>,
    provider_model_discoveries_pending: HashSet<ProviderKind>,
    /// CLI version per provider, probed off-thread. Missing key means the
    /// probe has not answered yet; `None` means it ran and found nothing.
    provider_versions: HashMap<ProviderKind, Option<String>>,
    provider_version_tx: Sender<(ProviderKind, Option<String>)>,
    provider_version_events: Receiver<(ProviderKind, Option<String>)>,
    /// Providers with a version probe in flight, so a re-detect cannot stack
    /// a second subprocess on one that has not answered.
    provider_version_probes_pending: HashSet<ProviderKind>,
    /// PATH re-detection results from the Providers page's refresh, merged
    /// into `probes` without touching their model catalogs.
    provider_detection_tx: Sender<(ProviderKind, bool, Option<PathBuf>)>,
    provider_detection_events: Receiver<(ProviderKind, bool, Option<PathBuf>)>,
    /// Providers the running re-detection has not answered for yet; empty
    /// means no re-detection is in flight.
    provider_detection_remaining: usize,
    /// When provider detection last completed, for the page's "Checked" label.
    provider_detection_checked_at: Option<Instant>,
    /// The provider row expanded on the Providers page, if any. The binary
    /// override input below edits this provider's entry.
    expanded_provider_settings: Option<ProviderKind>,
    provider_path_input: Entity<ComposerInput>,
    computer_permissions: ComputerPermissions,
    computer_permission_tx: Sender<Result<ComputerPermissions, String>>,
    computer_permission_events: Receiver<Result<ComputerPermissions, String>>,
    computer_permission_request_pending: bool,
    /// Account rate-limit meters per provider, fetched off-thread (Claude and
    /// Codex over HTTPS, Grok through a stdio probe) and refreshed live by
    /// Codex's own stream. Frames read only this snapshot.
    plan_usage: HashMap<ProviderKind, crate::usage::PlanUsage>,
    /// Why a provider's last fetch failed, kept alongside stale data for the
    /// meter's tooltip. Cleared by that provider's next success.
    plan_usage_error: HashMap<ProviderKind, String>,
    plan_usage_tx: Sender<(ProviderKind, Result<crate::usage::PlanUsage, String>)>,
    plan_usage_events: Receiver<(ProviderKind, Result<crate::usage::PlanUsage, String>)>,
    plan_usage_pending: HashSet<ProviderKind>,
    /// When each provider's last fetch settled, successful or not — the
    /// refresh backoff measures from here.
    plan_usage_checked_at: HashMap<ProviderKind, Instant>,
    /// Providers whose turn settled since the last fetch, so the meters have
    /// moved.
    plan_usage_stale: HashSet<ProviderKind>,
    /// The settings Usage page's snapshot: historical token/cost usage
    /// scanned from provider transcripts off-thread. Frames read only this.
    usage_history: Option<crate::usage_history::UsageHistory>,
    /// The window a scan is currently in flight for, so a repeat request for
    /// the same window coalesces while a changed window supersedes it.
    usage_history_pending_for: Option<crate::usage_history::UsageWindow>,
    /// Bumped per scan; a result from a superseded scan is discarded.
    usage_history_generation: u64,
    /// When the current snapshot landed, for the reopen-staleness check.
    usage_history_scanned_at: Option<Instant>,
    /// Per-file parsed-record cache, locked only on the background executor.
    usage_scan_cache: std::sync::Arc<std::sync::Mutex<crate::usage_history::ScanCache>>,
    /// The LiteLLM rate table plus when it was loaded, shared with scans the
    /// same way; the TTL re-check happens inside the scan task.
    usage_rate_table:
        std::sync::Arc<std::sync::Mutex<Option<(Instant, crate::usage_history::RateTable)>>>,
    /// Directory holding the rate-table disk cache, beside the app database.
    usage_rates_dir: PathBuf,
    usage_view: UsageViewMode,
    /// The selected window for the daily and project views; the statement
    /// view fixes its own.
    usage_window: crate::usage_history::UsageWindow,
    usage_metric: UsageMetric,
    usage_breakdown: UsageBreakdown,
    /// Scroll position of the monthly statement card, which scrolls
    /// internally like the projects card so the two list views feel alike.
    usage_months_scroll: ScrollHandle,
    usage_months_scrollbar: Rc<ScrollbarState>,
    /// Filter query over the Usage page's project rows.
    usage_project_filter: Entity<ComposerInput>,
    /// Virtualized list over the filtered project rows, so only visible rows
    /// build elements no matter how many working directories have usage.
    usage_projects_list: ListState,
    usage_projects_scrollbar: Rc<ScrollbarState>,
    /// Indices into `usage_history.projects` the filter leaves visible — the
    /// row builder reads only this.
    usage_projects_rows: RefCell<Vec<usize>>,
    /// `(peak value, rank-by-cost)` for the visible rows' bars, refreshed
    /// once per frame rather than per row.
    usage_projects_scale: Cell<(f64, bool)>,
    /// Hovered or keyboard-selected day index on the Usage page's chart.
    usage_chart_hover: Option<usize>,
    /// The chart plot's window bounds, written during paint so the mouse-move
    /// handler can map positions to day indices.
    usage_chart_bounds: Rc<Cell<Option<gpui::Bounds<Pixels>>>>,
    computer_use_app_icons: RefCell<HashMap<String, Option<std::sync::Arc<gpui::Image>>>>,
    computer_use_app_icon_loads: RefCell<HashSet<String>>,
    model_picker_tab: ModelPickerTab,
    /// Keyboard cursor over the model picker's filtered rows. `None` means the
    /// keyboard has not moved yet, so `enter` takes the first row.
    model_picker_highlight: Option<usize>,
    model_picker_scroll: ScrollHandle,
    branch_search: Entity<ComposerInput>,
    branch_create_input: Entity<ComposerInput>,
    branch_picker_mode: BranchPickerMode,
    /// Keyboard cursor over the branch picker's enabled actions. Disabled
    /// rows remain visible but never enter this index.
    branch_picker_highlight: Option<usize>,
    branch_picker_list_state: ListState,
    branch_picker_row_cache: RefCell<Vec<crate::git_branch::BranchEntry>>,
    /// Git subprocess results per concrete workspace path. Render only reads
    /// this in-memory cache; misses are fulfilled on the background executor.
    branch_snapshots: QueryCache<PathBuf, Result<Option<BranchSnapshot>, String>>,
    /// Stale-while-revalidate value for the selected path, avoiding label
    /// flicker when app activation invalidates the query.
    visible_branch_snapshot: Option<(PathBuf, BranchSnapshot)>,
    branch_operation_pending: bool,
    /// Slash commands discovered per (provider, project root). Filesystem
    /// walks live on the background executor; frames read the index below.
    slash_commands: QueryCache<(ProviderKind, PathBuf), Vec<SlashCommand>>,
    /// The merged command list the autocomplete popup draws, and the key it
    /// was built for — a stale key means "no commands", never another
    /// provider's list.
    slash_command_index: Rc<Vec<SlashCommand>>,
    slash_command_index_key: Option<(ProviderKind, PathBuf)>,
    /// Workspace file index per project root, for `@` mentions.
    mention_files: QueryCache<PathBuf, Vec<FileEntry>>,
    mention_file_index: Rc<Vec<FileEntry>>,
    mention_file_index_path: Option<PathBuf>,
    /// Set when a driver reports its command registry mid-drain; the drain
    /// has no `Context` to rebuild the drawn index itself.
    composer_sources_stale: bool,
    composer_autocomplete: autocomplete::AutocompleteUi,
    /// Files dropped onto the composer, drawn as chips above the input and
    /// drained into the next submission.
    composer_attachments: Vec<ComposerAttachment>,
    runtimes: HashMap<Uuid, SessionRuntime>,
    /// Accepted submissions still creating their workspace/checkpoint. The
    /// session is busy immediately, while the composer draws a spinner until
    /// the provider runtime exists and cancellation is wired.
    submission_preparations: HashSet<Uuid>,
    /// Sessions whose just-settled turn should start the next queued
    /// follow-up. Processed at the end of the driver-event drain so the
    /// session's runtime has already been re-inserted before a new prompt
    /// reuses it.
    pending_queue_drains: Vec<Uuid>,
    stream_state_dirty: bool,
    last_stream_save: Instant,
    /// User expansion overrides keyed by persisted transcript block index.
    reasoning_expanded: HashMap<usize, bool>,
    activities_expanded: HashMap<usize, bool>,
    /// Individual tool rows the user has opened to read their full detail.
    expanded_activity_items: HashSet<Uuid>,
    /// Settled turns whose folded work the user has reopened.
    expanded_turns: HashSet<Uuid>,
    session_navigation: SessionNavigation,
    sidebar_visible: bool,
    sidebar_width: f32,
    right_panel_visible: bool,
    right_panel_width: f32,
    fps_counter_visible: bool,
    panel_resize_drag: Option<PanelResizeDrag>,
    right_panel_session_states: HashMap<Uuid, RightPanelSessionState>,
    right_panel_surfaces: Vec<RightPanelSurface>,
    right_panel_active_surface: Option<usize>,
    right_panel_tabs_scroll_handle: ScrollHandle,
    right_panel_files_scroll_handle: ScrollHandle,
    right_panel_files_scrollbar: Rc<ScrollbarState>,
    right_panel_diff_scroll_handle: ScrollHandle,
    right_panel_diff_scrollbar: Rc<ScrollbarState>,
    right_panel_editor_scroll_handle: ScrollHandle,
    right_panel_editor_scrollbar: Rc<ScrollbarState>,
    right_panel_pending_tab_reveal: Option<usize>,
    right_panel_pending_terminal_focus: Option<Uuid>,
    right_panel_expanded_paths: HashSet<PathBuf>,
    right_panel_files_selected_path: Option<String>,
    right_panel_file_tree_width: f32,
    right_panel_file_editors: HashMap<String, RightPanelFileEditor>,
    /// Find-and-replace over the visible file editor. Created on first
    /// `cmd-f` and kept for the window's lifetime so the query and toggles
    /// survive closing the bar; `open` inside says whether it is showing.
    file_search: Option<file_search::FileSearch>,
    right_panel_diff_files: Vec<RightPanelDiffFile>,
    /// The working tree as currently drawn. Held so a refresh can redraw the
    /// previous listing instead of blanking the panel.
    right_panel_working_tree: Vec<right_panel::WorkingTreeEntry>,
    /// Working tree per project path. Walking it is filesystem I/O and must
    /// never happen in a frame.
    working_trees: QueryCache<PathBuf, Vec<right_panel::WorkingTreeEntry>>,
    /// Set when a turn finishes; the drain loop drops the workspace queries,
    /// since the event handler has no `Context` to refresh them itself.
    workspace_queries_stale: bool,
    /// Diff listing per project path, so a slow refresh cannot land on top of a
    /// newer one.
    right_panel_diffs: QueryCache<PathBuf, Vec<RightPanelDiffFile>>,
    right_panel_terminals: HashMap<Uuid, Entity<TerminalView>>,
    right_panel_browsers: HashMap<Uuid, Entity<BrowserView>>,
    /// A Browser surface was just opened; the next right panel render moves
    /// focus into its address bar.
    right_panel_pending_browser_focus: Option<Uuid>,
    /// GPUI is compositing deferred draws on a plane above native views, so
    /// menus render over the live webview and no snapshot occlusion is needed.
    /// When the overlay could not be enabled, the browser falls back to
    /// swapping in frozen page pixels while an overlay is open.
    scene_overlay_enabled: bool,
    settings_page: Option<SettingsPage>,
    /// Scroll position of the settings content column, tracked so the pane
    /// can draw a scrollbar and mark the titlebar boundary once content
    /// slides under it.
    settings_scroll: ScrollHandle,
    settings_scrollbar: Rc<ScrollbarState>,
    header_drag_armed: bool,
    toast: Option<ToastState>,
    toast_generation: u64,
    copied_message_feedback: HashMap<Uuid, u64>,
    copied_message_generation: u64,
    copied_activity_feedback: HashMap<(Uuid, ActivityDisclosureSectionKind), u64>,
    copied_activity_generation: u64,
    message_edit: Option<MessageEdit>,
    transcript_rows: ListState,
    /// Active turns use top alignment so row remeasurement cannot invoke the
    /// bottom-aligned list's implicit pin and displace the sent-message anchor.
    anchored_transcript_rows: ListState,
    /// Virtualized list backing the sidebar session history, so only visible
    /// rows are built and laid out regardless of how many sessions exist.
    sidebar_list_state: ListState,
    sidebar_scrollbar: Rc<ScrollbarState>,
    /// Snapshot of the sidebar rows the list state currently corresponds to.
    sidebar_row_cache: RefCell<Vec<SidebarRow>>,
    transcript_row_kinds: RefCell<Vec<TranscriptRowKind>>,
    /// Fingerprint of the transcript inputs `transcript_row_kinds` was folded
    /// from, so an unchanged transcript costs nothing on a frame. `None` until
    /// the first fold. See `transcript_rows_fingerprint`.
    transcript_row_kinds_fingerprint: Cell<Option<u64>>,
    /// The navigation rail's turn list, shared by `Rc` so a frame hands the
    /// rail a pointer instead of re-extracting every turn's snippets. Rebuilt
    /// by `navigation_turns` when the row-kinds fingerprint moves.
    transcript_navigation_turns: RefCell<Rc<Vec<TranscriptNavigationTurn>>>,
    /// The row-kinds fingerprint `transcript_navigation_turns` was built from.
    transcript_navigation_turns_fingerprint: Cell<Option<u64>>,
    /// Checkpoint-ref existence per (session, retained turn count), filled by
    /// `prefetch_checkpoint_refs` on the background executor. Rows read only
    /// this cache: resolving a ref forks a `git` subprocess, which must stay
    /// off the frame path.
    checkpoint_ref_cache: RefCell<HashMap<(Uuid, usize), bool>>,
    /// Bumped whenever checkpoint refs may have changed. A prefetch launched
    /// under an older generation is stale and discarded on arrival.
    checkpoint_ref_generation: Cell<u64>,
    /// The (session, generation) the latest scheduled prefetch covers.
    checkpoint_ref_prefetch: Cell<Option<(Uuid, u64)>>,
    /// Turn checkpoints asked for but not started yet.
    ///
    /// `capture_turn` is upwards of ten `git` invocations, one of them a
    /// `git add -A` over the whole worktree, and the driver-event drain that
    /// asks for it shares the UI thread with rendering. Requests queue here and
    /// `start_pending_checkpoint_captures` runs them on the background executor.
    pending_checkpoint_captures: Vec<PendingCheckpointCapture>,
    /// The (session, turn) captures currently running, so a repeated request —
    /// a turn that finishes while its own capture is still going — does not
    /// fork a second `git add -A` over the same worktree.
    checkpoint_captures_in_flight: HashSet<(Uuid, usize)>,
    /// Clock for the idle-session sweep, so the check costs one comparison per
    /// frame instead of a scan.
    last_idle_session_sweep: Instant,
    transcript_anchor: Cell<Option<TranscriptAnchor>>,
    transcript_anchor_end_space: Rc<Cell<Pixels>>,
    transcript_anchor_following: Rc<Cell<bool>>,
    transcript_is_scrolled: Rc<Cell<bool>>,
    transcript_layout_width: Cell<Pixels>,
    /// Parsed markdown per assistant message, keeping each response's
    /// incremental parse and flattened blocks alive across frames.
    message_markdown: RefCell<HashMap<Uuid, MarkdownView>>,
    /// Parsed markdown for reasoning blocks, keyed by transcript block index.
    block_markdown: RefCell<HashMap<usize, MarkdownView>>,
    /// Transcript-wide text selection, spanning messages and tool output.
    transcript_selection: TranscriptSelection,
    /// Independent selection for the transient toast message. Keeping it out
    /// of the transcript registry prevents an overlay from joining a drag to
    /// whatever happens to be painted beneath it.
    toast_selection: TranscriptSelection,
    transcript_scrollbar: Rc<ScrollbarState>,
    /// Every menu site in the app, keyed by a stable id. Handles are created on
    /// first use and live as long as the window.
    menus: RefCell<HashMap<SharedString, ContextMenuHandle>>,
    navigation_rail: Entity<ConversationNavigationRail>,
    navigation_rail_active_scale_enabled: Rc<Cell<bool>>,
    navigation_rail_reset_generation: Cell<u64>,
    /// Whether the once-per-second "Working for Ns" notify loop is live, so a
    /// frame can ensure it without stacking a second loop.
    working_elapsed_ticker_running: Cell<bool>,
    /// Live frames-per-second measurement for the header counter.
    fps_last_frame: Instant,
    fps_frame_count: u64,
    fps_value: u32,
}

mod autocomplete;
mod branches;
mod components;
mod composer;
mod file_search;
mod render;
mod right_panel;
mod runtime;
mod sessions;
mod settings;
mod sidebar;
mod streaming;
mod transcript;
mod transcript_view;
mod usage_meter;
mod usage_page;

pub use autocomplete::init as init_composer_autocomplete;
use components::*;
pub use settings::init as init_settings_keys;
use sidebar::SidebarRow;
use streaming::*;
use transcript::*;
use transcript_view::ConversationNavigationRail;

fn migrate_legacy_projectless_projects(state: &mut PersistedState) -> std::io::Result<bool> {
    let legacy_indices = state
        .projects
        .iter()
        .enumerate()
        .filter_map(|(index, project)| {
            crate::projectless::is_legacy_root_path(&project.path).then_some(index)
        })
        .collect::<Vec<_>>();
    if legacy_indices.is_empty() {
        return Ok(false);
    }

    // Allocate everything first so a later failure never leaves only part of
    // the in-memory project list rewritten.
    let workspaces = legacy_indices
        .iter()
        .map(|_| crate::projectless::create_workspace(None))
        .collect::<std::io::Result<Vec<_>>>()?;
    for (index, workspace) in legacy_indices.into_iter().zip(workspaces) {
        state.projects[index].name = Project::PROJECTLESS_NAME.to_owned();
        state.projects[index].path = workspace.cwd;
    }
    Ok(true)
}

impl Waku {
    pub(super) fn show_toast(&mut self, message: impl Into<String>) {
        self.toast_selection.selection.borrow_mut().clear();
        self.toast_selection.registry.borrow_mut().clear();
        self.toast_generation = self.toast_generation.wrapping_add(1);
        self.toast = Some(ToastState {
            message: message.into(),
            id: self.toast_generation,
            timer_generation: self.toast_generation,
            duration_remaining: DEFAULT_TOAST_DURATION,
            timer_started: None,
            hovered: false,
        });
    }

    pub(super) fn hide_toast(&mut self) {
        if self.toast.take().is_some() {
            self.toast_selection.selection.borrow_mut().clear();
            self.toast_selection.registry.borrow_mut().clear();
            // Detached timers are deliberately cheap, but their generation
            // must stop them from dismissing a newer toast.
            self.toast_generation = self.toast_generation.wrapping_add(1);
        }
    }

    fn start_toast_dismiss_timer(&mut self, cx: &mut Context<Self>) {
        let Some(toast) = self.toast.as_mut() else {
            return;
        };
        if toast.hovered || toast.timer_started.is_some() {
            return;
        }

        let duration = toast.duration_remaining;
        let generation = toast.timer_generation;
        toast.timer_started = Some(Instant::now());
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(duration).await;
            let _ = this.update(cx, |this, cx| {
                if this
                    .toast
                    .as_ref()
                    .is_some_and(|toast| toast.timer_generation == generation && !toast.hovered)
                {
                    this.hide_toast();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn set_toast_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        let Some(toast) = self.toast.as_ref() else {
            return;
        };
        if toast.hovered == hovered {
            return;
        }

        self.toast_generation = self.toast_generation.wrapping_add(1);
        let generation = self.toast_generation;
        let toast = self.toast.as_mut().expect("toast checked above");
        toast.timer_generation = generation;
        toast.hovered = hovered;
        if hovered {
            if let Some(started) = toast.timer_started.take() {
                toast.duration_remaining =
                    paused_toast_duration(toast.duration_remaining, started.elapsed());
            }
        } else {
            toast.timer_started = None;
            self.start_toast_dismiss_timer(cx);
        }
    }

    pub fn new(window: &mut Window, cx: &mut App) -> Entity<Self> {
        let composer = cx.new(|cx| ComposerInput::new(window, cx));
        let model_search = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder("Search models...")
        });
        let branch_search = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder("Search branches")
        });
        let branch_create_input = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder("New branch name")
        });
        let settings_search = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder("Search Settings")
        });
        let provider_path_input = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .select_all_on_focus_click()
                .placeholder("Detected automatically")
        });
        let usage_project_filter = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder("Filter projects")
        });
        let navigation_rail = cx.new(|_| ConversationNavigationRail::new());
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let store = StateStore::new(StateStore::default_path());
        let mut state = store.load_or_fresh(cwd);
        let startup_toast = match migrate_legacy_projectless_projects(&mut state) {
            Ok(false) => None,
            Ok(true) => store
                .save(&mut state)
                .err()
                .map(|error| format!("Could not save the projectless task migration: {error}")),
            Err(error) => Some(format!(
                "Could not move the old root-level projectless task beneath ~/.waku: {error}"
            )),
        };
        let sidebar_visible = state.sidebar_visible;
        let right_panel_visible = state.right_panel_visible;
        let sidebar_width = sanitize_panel_width(
            state.sidebar_width,
            DEFAULT_SIDEBAR_WIDTH,
            SIDEBAR_MIN_WIDTH,
            SIDEBAR_MAX_WIDTH,
        );
        let right_panel_width = sanitize_panel_width(
            state.right_panel_width,
            DEFAULT_RIGHT_PANEL_WIDTH,
            RIGHT_PANEL_MIN_WIDTH,
            RIGHT_PANEL_MAX_WIDTH,
        );
        state.sidebar_width = sidebar_width;
        state.right_panel_width = right_panel_width;
        crate::theme::apply_theme_preference(state.theme, window, cx);
        crate::platform::set_sidebar_material_width(window, sidebar_width);
        let project_paths = state
            .projects
            .iter()
            .map(|project| (project.id, project.path.clone()))
            .collect::<HashMap<_, _>>();
        let mut interrupted_turn_checkpoints = Vec::new();
        for session in &mut state.sessions {
            session.migrate_legacy_state();
            if session.status != SessionStatus::Idle {
                session.status = SessionStatus::Idle;
            }
            let interrupted_turn = if let Some(turn) = session
                .turns
                .last_mut()
                .filter(|turn| turn.status == TurnStatus::Running)
            {
                turn.status = TurnStatus::Interrupted;
                turn.completed_at = Some(unix_time());
                Some(turn.turn_count)
            } else {
                None
            };
            // A crash mid-turn leaves work in the tree worth checkpointing, but
            // one `capture_turn` per interrupted session is upwards of ten
            // `git` invocations each — paid here, before the window has drawn
            // once. Queue them and let the first frames go out first.
            if let Some(turn_count) = interrupted_turn
                && let Some(project_path) = session
                    .workspace
                    .path()
                    .map(std::path::Path::to_path_buf)
                    .or_else(|| project_paths.get(&session.project_id).cloned())
            {
                interrupted_turn_checkpoints.push(PendingCheckpointCapture {
                    session_id: session.id,
                    turn_count,
                    project_path,
                });
            }
            for message in &mut session.messages {
                message.streaming = false;
            }
            session.transcript_blocks.retain(|block| {
                !matches!(
                    &block.content,
                    TranscriptBlockContent::Reasoning(reasoning)
                        if reasoning.content.trim().is_empty()
                )
            });
            for block in &mut session.transcript_blocks {
                if let TranscriptBlockContent::Activities(activities) = &mut block.content {
                    for activity in activities {
                        activity.complete = true;
                    }
                }
            }
        }
        let probes = ProviderKind::ALL
            .into_iter()
            .map(
                |provider| match state.provider_binary_overrides.get(&provider) {
                    Some(binary) => ProviderProbe::with_binary_override(provider, binary),
                    None => ProviderProbe::pending(provider),
                },
            )
            .collect::<Vec<_>>();
        let (provider_probe_tx, provider_probe_events) = unbounded();
        let (provider_version_tx, provider_version_events) = unbounded();
        let (provider_detection_tx, provider_detection_events) = unbounded();
        let (computer_permission_tx, computer_permission_events) = unbounded();
        let (plan_usage_tx, plan_usage_events) = unbounded();
        {
            let computer_permission_tx = computer_permission_tx.clone();
            std::thread::Builder::new()
                .name("waku-computer-permission-probe".into())
                .spawn(move || {
                    let result = crate::computer_use::probe_permissions(false)
                        .map_err(|error| error.to_string());
                    let _ = computer_permission_tx.send(result);
                })
                .ok();
        }
        let model_picker_tab = ModelPickerTab::Provider(
            state
                .selected_session
                .and_then(|id| state.sessions.iter().find(|session| session.id == id))
                .map(|session| session.provider)
                .unwrap_or(state.last_provider),
        );
        // Measure visible rows only, with a generous overdraw — the same shape
        // Zed's own agent chat uses. `measure_all` lays out every row in the
        // session on the first frame and again after any structural splice,
        // which a long transcript cannot afford.
        let transcript_rows = ListState::new(0, ListAlignment::Bottom, px(2048.0));
        let anchored_transcript_rows = ListState::new(0, ListAlignment::Top, px(2048.0));
        let sidebar_list_state = ListState::new(0, ListAlignment::Top, px(256.0));
        let usage_projects_list = ListState::new(0, ListAlignment::Top, px(256.0));
        let branch_picker_list_state = ListState::new(0, ListAlignment::Top, px(152.0));
        let transcript_is_scrolled = Rc::new(Cell::new(false));
        let transcript_anchor_following = Rc::new(Cell::new(false));
        let navigation_rail_active_scale_enabled = Rc::new(Cell::new(false));
        transcript_rows.set_scroll_handler({
            let transcript_is_scrolled = transcript_is_scrolled.clone();
            let transcript_anchor_following = transcript_anchor_following.clone();
            let navigation_rail_active_scale_enabled = navigation_rail_active_scale_enabled.clone();
            move |event, window, _| {
                transcript_is_scrolled.set(event.is_scrolled);
                transcript_anchor_following.set(false);
                if event.is_scrolled {
                    navigation_rail_active_scale_enabled.set(true);
                }
                window.refresh();
            }
        });
        anchored_transcript_rows.set_scroll_handler({
            let transcript_is_scrolled = transcript_is_scrolled.clone();
            let transcript_anchor_following = transcript_anchor_following.clone();
            let navigation_rail_active_scale_enabled = navigation_rail_active_scale_enabled.clone();
            move |event, window, _| {
                transcript_is_scrolled.set(event.is_scrolled);
                transcript_anchor_following.set(false);
                if event.is_scrolled {
                    navigation_rail_active_scale_enabled.set(true);
                }
                window.refresh();
            }
        });
        // Enable GPUI's experimental overlay plane so deferred draws (menus,
        // tooltips, popovers) composite above native child views — without it
        // the browser surface's WKWebView would cover them.
        let scene_overlay_enabled = window.enable_scene_overlay().is_ok();
        let entity = cx.new(|cx| {
            let settings_focus = cx.focus_handle();
            let onboarding_add_project_focus = cx.focus_handle();
            let onboarding_projectless_focus = cx.focus_handle();

            cx.observe_window_appearance(window, |this: &mut Self, window, cx| {
                if this.state.theme == ThemePreference::System {
                    crate::theme::apply_theme_preference(this.state.theme, window, cx);
                    cx.notify();
                }
            })
            .detach();

            cx.observe_window_activation(window, |this: &mut Self, window, cx| {
                if window.is_window_active() {
                    this.reload_clean_right_panel_file_editors(cx);
                    // The working tree and branch may have moved while another
                    // app had focus — a checkout in a terminal, an edit in an
                    // editor. Coming back is the moment to re-check.
                    this.invalidate_workspace_queries(cx);
                    if this.settings_page == Some(SettingsPage::ComputerUse) {
                        this.request_computer_permissions(false, cx);
                    }
                }
            })
            .detach();

            // A closed surface can take the window's focus down with it —
            // closing a browser tab drops the focused address input — and
            // with nothing focused, action availability walks only the root
            // dispatch node, so every app menu item greys out. When focus
            // dies with its element, send it home to the composer, the way
            // Zed's workspace refocuses itself.
            cx.on_focus_lost(window, |this: &mut Self, window, cx| {
                let focus = this.composer_focus(cx);
                window.focus(&focus, cx);
            })
            .detach();

            // Edits, not raw notifies: a field also notifies for caret blinks
            // and selection changes, and none of the app chrome depends on
            // those — re-rendering the window twice a second for a blinking
            // caret is exactly what the Performance guidance forbids.
            cx.subscribe(
                &composer,
                |this: &mut Self, _, event: &ComposerEvent, cx| match event {
                    ComposerEvent::Submit(prompt) => {
                        if let Some(prompt) = this.submission_with_attachments(prompt) {
                            this.submit_prompt(prompt, cx);
                        }
                    }
                    ComposerEvent::SubmitSteer(prompt) => {
                        if let Some(prompt) = this.submission_with_attachments(prompt) {
                            this.steer_prompt(prompt, cx);
                        }
                    }
                    ComposerEvent::Edited => cx.notify(),
                    ComposerEvent::Focus => {}
                    ComposerEvent::BackspaceOnEmpty => {
                        if this.composer_attachments.pop().is_some() {
                            cx.notify();
                        }
                    }
                },
            )
            .detach();

            // A changed query re-filters the picker rows and renumbers them,
            // so the drawn selection cannot carry over. While a filter is
            // active the cursor lands on the first match so `enter` has a
            // visible target; clearing the query returns to the opening
            // state — nothing highlighted, the current model's row in view.
            cx.subscribe(
                &model_search,
                |this: &mut Self, search, event: &ComposerEvent, cx| {
                    if matches!(event, ComposerEvent::Edited) {
                        if search.read(cx).content().trim().is_empty() {
                            this.model_picker_highlight = None;
                            this.reveal_selected_picker_model();
                        } else {
                            this.model_picker_highlight = Some(0);
                            this.model_picker_scroll.scroll_to_item(0);
                        }
                        cx.notify();
                    }
                },
            )
            .detach();
            cx.subscribe(
                &branch_search,
                |this: &mut Self, search, event: &ComposerEvent, cx| {
                    if matches!(event, ComposerEvent::Edited)
                        && this.branch_picker_mode == BranchPickerMode::Browse
                    {
                        if search.read(cx).content().trim().is_empty() {
                            this.branch_picker_highlight = None;
                        } else {
                            this.branch_picker_highlight = Some(0);
                            this.branch_picker_list_state.scroll_to_reveal_item(0);
                        }
                        cx.notify();
                    }
                },
            )
            .detach();
            cx.subscribe(
                &branch_create_input,
                |_: &mut Self, _, event: &ComposerEvent, cx| {
                    if matches!(event, ComposerEvent::Edited) {
                        cx.notify();
                    }
                },
            )
            .detach();
            cx.subscribe(
                &settings_search,
                |_: &mut Self, _, event: &ComposerEvent, cx| {
                    if matches!(event, ComposerEvent::Edited) {
                        cx.notify();
                    }
                },
            )
            .detach();
            cx.subscribe(
                &usage_project_filter,
                |_: &mut Self, _, event: &ComposerEvent, cx| {
                    if matches!(event, ComposerEvent::Edited) {
                        cx.notify();
                    }
                },
            )
            .detach();
            cx.subscribe(
                &provider_path_input,
                |this: &mut Self, _, event: &ComposerEvent, cx| {
                    if matches!(event, ComposerEvent::Submit(_)) {
                        this.apply_provider_path_override(cx);
                    }
                },
            )
            .detach();

            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(STREAM_FRAME_INTERVAL).await;
                    if this
                        .update(cx, |this, cx| {
                            // `|` on purpose: every drain runs every tick. A
                            // short-circuit would let a busy stream (whose
                            // drain reports a change each tick) starve the
                            // later queues for as long as it runs.
                            if this.drain_driver_events(cx)
                                | this.drain_provider_probe_events()
                                | this.drain_provider_version_events()
                                | this.drain_provider_detection_events()
                                | this.drain_computer_permission_events()
                                | this.drain_plan_usage_events()
                            {
                                cx.notify();
                            }
                            this.maybe_refresh_plan_usage(cx);
                            if std::mem::take(&mut this.workspace_queries_stale) {
                                this.invalidate_workspace_queries(cx);
                            }
                            if std::mem::take(&mut this.composer_sources_stale) {
                                this.refresh_composer_sources(cx);
                            }
                            this.reap_idle_sessions();
                            // A finished turn asks for a checkpoint from a
                            // handler with no `Context`; this is where that
                            // `git` work actually leaves the UI thread.
                            this.start_pending_checkpoint_captures(cx);
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();

            Self {
                state,
                store,
                composer,
                model_search,
                branch_search,
                branch_create_input,
                settings_search,
                settings_focus,
                onboarding_add_project_focus,
                onboarding_projectless_focus,
                automatic_updates_enabled: cx
                    .try_global::<crate::updater::UpdaterState>()
                    .and_then(|updater| updater.0.as_ref())
                    .is_some_and(|updater| updater.automatically_checks_for_updates()),
                probes,
                provider_probe_tx,
                provider_probe_events,
                provider_model_discoveries: HashSet::new(),
                provider_model_discoveries_pending: HashSet::new(),
                provider_versions: HashMap::new(),
                provider_version_tx,
                provider_version_events,
                provider_version_probes_pending: HashSet::new(),
                provider_detection_tx,
                provider_detection_events,
                provider_detection_remaining: 0,
                // Startup ran the same PATH detection synchronously just above.
                provider_detection_checked_at: Some(Instant::now()),
                expanded_provider_settings: None,
                provider_path_input,
                computer_permissions: ComputerPermissions::default(),
                computer_permission_tx,
                computer_permission_events,
                computer_permission_request_pending: false,
                plan_usage: HashMap::new(),
                plan_usage_error: HashMap::new(),
                plan_usage_tx,
                plan_usage_events,
                plan_usage_pending: HashSet::new(),
                plan_usage_checked_at: HashMap::new(),
                plan_usage_stale: HashSet::new(),
                usage_history: None,
                usage_history_pending_for: None,
                usage_history_generation: 0,
                usage_history_scanned_at: None,
                usage_scan_cache: std::sync::Arc::default(),
                usage_rate_table: std::sync::Arc::default(),
                usage_rates_dir: StateStore::default_path()
                    .parent()
                    .map(|directory| directory.to_owned())
                    .unwrap_or_else(std::env::temp_dir),
                usage_view: UsageViewMode::Daily,
                usage_window: crate::usage_history::UsageWindow::TrailingDays(30),
                usage_metric: UsageMetric::Cost,
                usage_breakdown: UsageBreakdown::Model,
                usage_months_scroll: ScrollHandle::new(),
                usage_months_scrollbar: ScrollbarState::new(),
                usage_project_filter,
                usage_projects_list,
                usage_projects_scrollbar: ScrollbarState::new(),
                usage_projects_rows: RefCell::new(Vec::new()),
                usage_projects_scale: Cell::new((0.0, true)),
                usage_chart_hover: None,
                usage_chart_bounds: Rc::default(),
                computer_use_app_icons: RefCell::new(HashMap::new()),
                computer_use_app_icon_loads: RefCell::new(HashSet::new()),
                model_picker_tab,
                model_picker_highlight: None,
                model_picker_scroll: ScrollHandle::new(),
                branch_picker_mode: BranchPickerMode::Browse,
                branch_picker_highlight: None,
                branch_picker_list_state,
                branch_picker_row_cache: RefCell::new(Vec::new()),
                branch_snapshots: QueryCache::new(MAX_CACHED_WORKSPACES),
                visible_branch_snapshot: None,
                branch_operation_pending: false,
                // Providers × workspaces; both scans are small, the cache
                // only exists to keep them off the frame path.
                slash_commands: QueryCache::new(2 * MAX_CACHED_WORKSPACES),
                slash_command_index: Rc::new(Vec::new()),
                slash_command_index_key: None,
                mention_files: QueryCache::new(MAX_CACHED_WORKSPACES),
                mention_file_index: Rc::new(Vec::new()),
                mention_file_index_path: None,
                composer_sources_stale: false,
                composer_autocomplete: autocomplete::AutocompleteUi::new(),
                composer_attachments: Vec::new(),
                runtimes: HashMap::new(),
                submission_preparations: HashSet::new(),
                pending_queue_drains: Vec::new(),
                stream_state_dirty: false,
                last_stream_save: Instant::now(),
                reasoning_expanded: HashMap::new(),
                activities_expanded: HashMap::new(),
                expanded_activity_items: HashSet::new(),
                expanded_turns: HashSet::new(),
                session_navigation: SessionNavigation::default(),
                sidebar_visible,
                sidebar_width,
                right_panel_visible,
                right_panel_width,
                fps_counter_visible: false,
                panel_resize_drag: None,
                right_panel_session_states: HashMap::new(),
                right_panel_surfaces: Vec::new(),
                right_panel_active_surface: None,
                right_panel_tabs_scroll_handle: ScrollHandle::new(),
                right_panel_files_scroll_handle: ScrollHandle::new(),
                right_panel_files_scrollbar: ScrollbarState::new(),
                right_panel_diff_scroll_handle: ScrollHandle::new(),
                right_panel_diff_scrollbar: ScrollbarState::new(),
                right_panel_editor_scroll_handle: ScrollHandle::new(),
                right_panel_editor_scrollbar: ScrollbarState::new(),
                right_panel_pending_tab_reveal: None,
                right_panel_pending_terminal_focus: None,
                right_panel_expanded_paths: HashSet::new(),
                right_panel_files_selected_path: None,
                right_panel_file_tree_width: DEFAULT_FILE_TREE_WIDTH,
                right_panel_file_editors: HashMap::new(),
                file_search: None,
                right_panel_diff_files: Vec::new(),
                right_panel_working_tree: Vec::new(),
                working_trees: QueryCache::new(MAX_CACHED_WORKSPACES),
                workspace_queries_stale: false,
                right_panel_diffs: QueryCache::new(MAX_CACHED_WORKSPACES),
                right_panel_terminals: HashMap::new(),
                right_panel_browsers: HashMap::new(),
                right_panel_pending_browser_focus: None,
                scene_overlay_enabled,
                settings_page: None,
                settings_scroll: ScrollHandle::new(),
                settings_scrollbar: ScrollbarState::new(),
                header_drag_armed: false,
                toast: startup_toast.map(|message| ToastState {
                    message,
                    id: 0,
                    timer_generation: 0,
                    duration_remaining: DEFAULT_TOAST_DURATION,
                    timer_started: None,
                    hovered: false,
                }),
                toast_generation: 0,
                copied_message_feedback: HashMap::new(),
                copied_message_generation: 0,
                copied_activity_feedback: HashMap::new(),
                copied_activity_generation: 0,
                message_edit: None,
                transcript_rows,
                anchored_transcript_rows,
                sidebar_list_state,
                sidebar_scrollbar: ScrollbarState::new(),
                sidebar_row_cache: RefCell::new(Vec::new()),
                transcript_row_kinds: RefCell::new(Vec::new()),
                transcript_row_kinds_fingerprint: Cell::new(None),
                transcript_navigation_turns: RefCell::new(Rc::new(Vec::new())),
                transcript_navigation_turns_fingerprint: Cell::new(None),
                checkpoint_ref_cache: RefCell::new(HashMap::new()),
                checkpoint_ref_generation: Cell::new(0),
                checkpoint_ref_prefetch: Cell::new(None),
                pending_checkpoint_captures: interrupted_turn_checkpoints,
                checkpoint_captures_in_flight: HashSet::new(),
                last_idle_session_sweep: Instant::now(),
                transcript_anchor: Cell::new(None),
                transcript_anchor_end_space: Rc::new(Cell::new(Pixels::ZERO)),
                transcript_anchor_following,
                transcript_is_scrolled,
                transcript_layout_width: Cell::new(Pixels::ZERO),
                message_markdown: RefCell::new(HashMap::new()),
                block_markdown: RefCell::new(HashMap::new()),
                transcript_selection: TranscriptSelection::default(),
                toast_selection: TranscriptSelection::default(),
                transcript_scrollbar: ScrollbarState::new(),
                menus: RefCell::new(HashMap::new()),
                navigation_rail: navigation_rail.clone(),
                navigation_rail_active_scale_enabled,
                navigation_rail_reset_generation: Cell::new(0),
                working_elapsed_ticker_running: Cell::new(false),
                fps_last_frame: Instant::now(),
                fps_frame_count: 0,
                fps_value: 0,
            }
        });
        navigation_rail.update(cx, |rail, _| rail.set_waku(entity.downgrade()));
        let initial_row_count = entity.read(cx).transcript_row_count();
        entity.read(cx).reset_transcript_rows(initial_row_count);
        // Everything launch needs from `git` or the filesystem, started now
        // that there is an entity to notify and deliberately not before the
        // first frame.
        entity.update(cx, |this, cx| {
            this.start_pending_checkpoint_captures(cx);
            // The autocomplete indexes prefetch alongside, so typing `/` or
            // `@` into the very first prompt already has data to draw.
            this.refresh_composer_sources(cx);
            // Model discovery for every installed provider also starts here,
            // ahead of the model picker's first render, so opening it never
            // waits on a per-provider lazy load.
            this.request_all_model_discoveries();
            // CLI versions prefetch alongside for the same reason: the
            // Providers settings page reads only this store and must never
            // open onto a lazy load.
            this.request_provider_version_probes();
        });
        entity
    }
}

#[cfg(test)]
mod tests;
