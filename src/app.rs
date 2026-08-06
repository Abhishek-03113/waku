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
    Animation, AnimationExt, AnyElement, App, ClipboardItem, Context, Div, Entity, FocusHandle,
    Focusable, FontWeight, Hsla, IntoElement, KeyDownEvent, ListAlignment, ListOffset, ListState,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, NavigationDirection, ObjectFit,
    PathPromptOptions, Pixels, Render, ScrollHandle, SharedString, Stateful, StyleRefinement,
    WeakEntity, Window, canvas, div, ease_out_quint, fill, img, linear_color_stop, linear_gradient,
    list, point, prelude::*, pulsating_between, px, rgb,
};
use uuid::Uuid;

use crate::checkpoint;
use crate::composer_complete::{FileEntry, SlashCommand};
use crate::computer_use::{
    ComputerPermissions, ComputerUsePhase, ComputerUseState, PendingComputerApproval,
};
use crate::driver::{self, DriverHandle, DriverStartOptions, SessionOptions};
use crate::input::{ComposerEvent, ComposerInput};
use crate::md;
use crate::model::{
    ActivityItem, AgentSession, Checkpoint, CheckpointStatus, DriverEvent, FavoriteModel,
    InteractionMode, Message, MessageRole, PendingPermission, Project, ProviderKind, ProviderModel,
    ProviderProbe, ProviderResumeCursor, ReasoningBlock, RuntimeMode, SessionStatus,
    TranscriptBlock, TranscriptBlockContent, TurnStatus, compact_path, unix_time, unix_time_millis,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::md::render::{
    Ctx as MarkdownCtx, MarkdownView, Metrics as MarkdownMetrics, Palette as MarkdownPalette,
    TranscriptSelection,
};
use crate::ui::menu::{
    ConfirmEntry, ContextMenuHandle, DismissMenu, MenuAlign, MenuItem, SelectNextEntry,
    SelectPreviousEntry, context_menu, dropdown_menu, popover,
};
use crate::ui::scrollbar::{self, ScrollbarState};
use crate::ui::tooltip::Tooltip;

use crate::persistence::{
    DEFAULT_RIGHT_PANEL_WIDTH, DEFAULT_SIDEBAR_WIDTH, PersistedState, StateStore,
};
use crate::query::{Query, QueryCache};
use crate::terminal::TerminalView;
use crate::theme::{Theme, ThemePreference};
use crate::ui::{
    MenuChip, ProjectNameSelector, activity_icon, activity_noun, icon, icon_button, provider_color,
    provider_icon, status_color, status_label,
};
use crate::{
    CancelTurn, CloseFind, CloseWindow, CopySelection, FindNext, FindPrevious, FocusComposer,
    NavigateBack, NavigateForward, NewSession, OpenFind, OpenFindReplace, OpenSettings,
    ReplaceAllMatches, SaveFile, ToggleFindCaseSensitive, ToggleFindRegex, ToggleFindWholeWord,
    ToggleFpsCounter, ToggleModelPicker, ToggleRightPanel, ToggleSidebar,
};

const TRAFFIC_LIGHT_CLEARANCE: f32 = 86.0;
const CONTENT_MAX_WIDTH: f32 = 720.0;
/// Menu-registry id of the composer's model picker, shared by its render site
/// and the `cmd-/` toggle action.
const MODEL_PICKER_MENU_ID: &str = "provider-model-picker";
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SettingsPage {
    General,
    ComputerUse,
    Appearance,
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
    probes: Vec<ProviderProbe>,
    provider_probe_tx: Sender<ProviderProbe>,
    provider_probe_events: Receiver<ProviderProbe>,
    provider_model_discoveries: HashSet<ProviderKind>,
    provider_model_discoveries_pending: HashSet<ProviderKind>,
    computer_permissions: ComputerPermissions,
    computer_permission_tx: Sender<Result<ComputerPermissions, String>>,
    computer_permission_events: Receiver<Result<ComputerPermissions, String>>,
    computer_permission_request_pending: bool,
    computer_use_app_icons: RefCell<HashMap<String, Option<std::sync::Arc<gpui::Image>>>>,
    computer_use_app_icon_loads: RefCell<HashSet<String>>,
    model_picker_tab: ModelPickerTab,
    /// Keyboard cursor over the model picker's filtered rows. `None` means the
    /// keyboard has not moved yet, so `enter` takes the first row.
    model_picker_highlight: Option<usize>,
    model_picker_scroll: ScrollHandle,
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
    runtimes: HashMap<Uuid, SessionRuntime>,
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
    settings_page: Option<SettingsPage>,
    header_drag_armed: bool,
    branch: Option<String>,
    /// Branch per project path. `git` is too slow for the UI thread.
    branches: QueryCache<PathBuf, Option<String>>,
    toast: Option<String>,
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
    transcript_scrollbar: Rc<ScrollbarState>,
    /// Every menu site in the app, keyed by a stable id. Handles are created on
    /// first use and live as long as the window.
    menus: RefCell<HashMap<SharedString, ContextMenuHandle>>,
    navigation_rail: Entity<ConversationNavigationRail>,
    navigation_rail_active_scale_enabled: Rc<Cell<bool>>,
    navigation_rail_reset_generation: Cell<u64>,
    /// Live frames-per-second measurement for the header counter.
    fps_last_frame: Instant,
    fps_frame_count: u64,
    fps_value: u32,
}

mod autocomplete;
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

pub use autocomplete::init as init_composer_autocomplete;
use components::*;
use sidebar::SidebarRow;
use streaming::*;
use transcript::*;
use transcript_view::ConversationNavigationRail;

impl Waku {
    pub fn new(window: &mut Window, cx: &mut App) -> Entity<Self> {
        let composer = cx.new(|cx| ComposerInput::new(window, cx));
        let model_search =
            cx.new(|cx| ComposerInput::new(window, cx).placeholder("Search models..."));
        let settings_search =
            cx.new(|cx| ComposerInput::new(window, cx).placeholder("Search Settings"));
        let navigation_rail = cx.new(|_| ConversationNavigationRail::new());
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let store = StateStore::new(StateStore::default_path());
        let mut state = store.load_or_fresh(cwd);
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
                && let Some(project_path) = project_paths.get(&session.project_id)
            {
                interrupted_turn_checkpoints.push(PendingCheckpointCapture {
                    session_id: session.id,
                    turn_count,
                    project_path: project_path.clone(),
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
            .map(ProviderProbe::pending)
            .collect::<Vec<_>>();
        let (provider_probe_tx, provider_probe_events) = unbounded();
        let (computer_permission_tx, computer_permission_events) = unbounded();
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
        // The branch is a query like any other: `git branch --show-current` is
        // a subprocess, and running it here would hold the first frame for it.
        // `refresh_branch` below fills the cache off-thread, and the header
        // simply has no branch to draw until it lands.
        let branch = None;
        let branches = QueryCache::new(MAX_CACHED_WORKSPACES);
        // Measure visible rows only, with a generous overdraw — the same shape
        // Zed's own agent chat uses. `measure_all` lays out every row in the
        // session on the first frame and again after any structural splice,
        // which a long transcript cannot afford.
        let transcript_rows = ListState::new(0, ListAlignment::Bottom, px(2048.0));
        let anchored_transcript_rows = ListState::new(0, ListAlignment::Top, px(2048.0));
        let sidebar_list_state = ListState::new(0, ListAlignment::Top, px(256.0));
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
        let entity = cx.new(|cx| {
            let settings_focus = cx.focus_handle();

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

            // Edits, not raw notifies: a field also notifies for caret blinks
            // and selection changes, and none of the app chrome depends on
            // those — re-rendering the window twice a second for a blinking
            // caret is exactly what the Performance guidance forbids.
            cx.subscribe(
                &composer,
                |this: &mut Self, _, event: &ComposerEvent, cx| match event {
                    ComposerEvent::Submit(prompt) => this.submit_prompt(prompt.clone(), cx),
                    ComposerEvent::Edited => cx.notify(),
                    ComposerEvent::Focus => {}
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
                &settings_search,
                |_: &mut Self, _, event: &ComposerEvent, cx| {
                    if matches!(event, ComposerEvent::Edited) {
                        cx.notify();
                    }
                },
            )
            .detach();

            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(STREAM_FRAME_INTERVAL).await;
                    if this
                        .update(cx, |this, cx| {
                            if this.drain_driver_events()
                                || this.drain_provider_probe_events()
                                || this.drain_computer_permission_events()
                            {
                                cx.notify();
                            }
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
                settings_search,
                settings_focus,
                probes,
                provider_probe_tx,
                provider_probe_events,
                provider_model_discoveries: HashSet::new(),
                provider_model_discoveries_pending: HashSet::new(),
                computer_permissions: ComputerPermissions::default(),
                computer_permission_tx,
                computer_permission_events,
                computer_permission_request_pending: false,
                computer_use_app_icons: RefCell::new(HashMap::new()),
                computer_use_app_icon_loads: RefCell::new(HashSet::new()),
                model_picker_tab,
                model_picker_highlight: None,
                model_picker_scroll: ScrollHandle::new(),
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
                runtimes: HashMap::new(),
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
                settings_page: None,
                header_drag_armed: false,
                branch,
                branches,
                toast: None,
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
                transcript_scrollbar: ScrollbarState::new(),
                menus: RefCell::new(HashMap::new()),
                navigation_rail: navigation_rail.clone(),
                navigation_rail_active_scale_enabled,
                navigation_rail_reset_generation: Cell::new(0),
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
        // first frame: checkpoints for turns a previous run left running, and
        // the selected project's branch.
        entity.update(cx, |this, cx| {
            this.start_pending_checkpoint_captures(cx);
            this.refresh_branch(cx);
            // The autocomplete indexes prefetch alongside, so typing `/` or
            // `@` into the very first prompt already has data to draw.
            this.refresh_composer_sources(cx);
            // Model discovery for every installed provider also starts here,
            // ahead of the model picker's first render, so opening it never
            // waits on a per-provider lazy load.
            this.request_all_model_discoveries();
        });
        entity
    }
}

#[cfg(test)]
mod tests;
