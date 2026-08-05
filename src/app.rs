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
    Anchor, Animation, AnimationExt, AnyElement, App, ClipboardItem, Context, Div, Entity,
    FocusHandle, Focusable, FontWeight, Hsla, IntoElement, KeyDownEvent, ListAlignment, ListOffset,
    ListState, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, NavigationDirection,
    ObjectFit, PathPromptOptions, Pixels, Render, ScrollHandle, SharedString, Stateful,
    StyleRefinement, WeakEntity, Window, canvas, div, ease_out_quint, fill, img,
    linear_color_stop, linear_gradient, list, point, prelude::*, pulsating_between, px, rems, rgb,
};
use uuid::Uuid;

use crate::checkpoint;
use crate::computer_use::{
    ComputerPermissions, ComputerUsePhase, ComputerUseState, PendingComputerApproval,
};
use crate::driver::{self, DriverHandle, DriverStartOptions};
use crate::input::{ComposerEvent, ComposerInput, preserve_composer_focus_for_context_menu};
use crate::model::{
    ActivityItem, AgentSession, Checkpoint, CheckpointStatus, DriverEvent, FavoriteModel,
    InteractionMode, Message, MessageRole, PendingPermission, Project, ProviderKind, ProviderModel,
    ProviderProbe, ProviderResumeCursor, ReasoningBlock, RuntimeMode, SessionStatus,
    TranscriptBlock, TranscriptBlockContent, TurnStatus, compact_path, unix_time, unix_time_millis,
};
use gpui_component::highlighter::Language;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::popover::Popover;
use gpui_component::scroll::ScrollableElement;
use gpui_component::text::{
    TextView, TextViewScrollViewport, TextViewState, TextViewStyle,
};
use gpui_component::tooltip::Tooltip;
use unicode_segmentation::UnicodeSegmentation;

use crate::persistence::{
    DEFAULT_RIGHT_PANEL_WIDTH, DEFAULT_SIDEBAR_WIDTH, PersistedState, StateStore,
};
use crate::terminal::TerminalView;
use crate::theme::{Theme, ThemePreference};
use crate::ui::{
    MenuChip, ProjectNameSelector, activity_icon, activity_noun, icon, provider_color,
    provider_icon, status_color, status_label,
};
use crate::{
    CancelTurn, CloseWindow, FocusComposer, NavigateBack, NavigateForward, NewSession,
    OpenSettings, SaveFile, ToggleRightPanel, ToggleSidebar,
};

const TRAFFIC_LIGHT_CLEARANCE: f32 = 86.0;
const CONTENT_MAX_WIDTH: f32 = 720.0;
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
const NAVIGATION_RAIL_VIEWPORT_HEIGHT_RATIO: f32 = 0.65;
const NAVIGATION_RAIL_TICK_WIDTH: f32 = 32.0;
const NAVIGATION_RAIL_TICK_HEIGHT: f32 = 2.0;
const NAVIGATION_RAIL_TICK_GAP: f32 = 10.0;
const NAVIGATION_RAIL_INACTIVE_OPACITY: f32 = 0.45;
const NAVIGATION_RAIL_TURN_HEIGHT: f32 = NAVIGATION_RAIL_TICK_HEIGHT + NAVIGATION_RAIL_TICK_GAP;
const NAVIGATION_RAIL_ANIMATION_DURATION: Duration = Duration::from_millis(300);
const STREAM_FRAME_INTERVAL: Duration = Duration::from_millis(24);
const STREAM_SAVE_INTERVAL: Duration = Duration::from_secs(1);
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

struct RightPanelFileEditor {
    state: Entity<InputState>,
    disk_content: String,
    writable: bool,
    dirty: bool,
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

fn traits_menu_label(theme: Theme, label: &'static str) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| {
        div()
            .w_full()
            .h(px(20.0))
            .px(px(4.0))
            .flex()
            .items_center()
            .text_size(px(10.5))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.text_tertiary)
            .child(label)
    })
    .disabled(true)
}

fn traits_menu_choice(
    theme: Theme,
    label: String,
    is_default: bool,
    is_selected: bool,
) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| {
        div()
            .w_full()
            .h(px(26.0))
            .px(px(6.0))
            .rounded(px(5.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_size(px(11.5))
                    .font_weight(if is_selected {
                        FontWeight::MEDIUM
                    } else {
                        FontWeight::NORMAL
                    })
                    .text_color(if is_selected {
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .child(label.clone()),
            )
            .when(is_default, |element| {
                element.child(
                    div()
                        .h(px(16.0))
                        .px(px(5.0))
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
    })
    .selected(is_selected)
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
    model_search: Entity<InputState>,
    settings_search: Entity<InputState>,
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
    panel_resize_drag: Option<PanelResizeDrag>,
    right_panel_session_states: HashMap<Uuid, RightPanelSessionState>,
    right_panel_surfaces: Vec<RightPanelSurface>,
    right_panel_active_surface: Option<usize>,
    right_panel_tabs_scroll_handle: ScrollHandle,
    right_panel_pending_tab_reveal: Option<usize>,
    right_panel_pending_terminal_focus: Option<Uuid>,
    right_panel_expanded_paths: HashSet<PathBuf>,
    right_panel_files_selected_path: Option<String>,
    right_panel_file_tree_width: f32,
    right_panel_file_editors: HashMap<String, RightPanelFileEditor>,
    right_panel_diff_files: Vec<RightPanelDiffFile>,
    right_panel_terminals: HashMap<Uuid, Entity<TerminalView>>,
    settings_page: Option<SettingsPage>,
    header_drag_armed: bool,
    branch: Option<String>,
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
    transcript_row_kinds: RefCell<Vec<TranscriptRowKind>>,
    transcript_anchor: Cell<Option<TranscriptAnchor>>,
    transcript_anchor_end_space: Rc<Cell<Pixels>>,
    transcript_anchor_following: Rc<Cell<bool>>,
    transcript_is_scrolled: Rc<Cell<bool>>,
    transcript_layout_width: Cell<Pixels>,
    message_text_states: HashMap<Uuid, Entity<TextViewState>>,
    activity_text_states:
        RefCell<HashMap<(Uuid, ActivityDisclosureSectionKind), Entity<TextViewState>>>,
    navigation_rail: Entity<ConversationNavigationRail>,
    navigation_rail_active_scale_enabled: Rc<Cell<bool>>,
    navigation_rail_reset_generation: Cell<u64>,
}

mod components;
mod composer;
mod render;
mod right_panel;
mod runtime;
mod sessions;
mod settings;
mod sidebar;
mod streaming;
mod transcript;
mod transcript_view;

use components::*;
use streaming::*;
use transcript::*;
use transcript_view::ConversationNavigationRail;

impl Waku {
    pub fn new(window: &mut Window, cx: &mut App) -> Entity<Self> {
        let composer = cx.new(|cx| ComposerInput::new(window, cx));
        let model_search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search models...")
                .clean_on_escape()
        });
        let settings_search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search Settings")
                .clean_on_escape()
        });
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
            if let Some(turn_count) = interrupted_turn
                && let Some(project_path) = project_paths.get(&session.project_id)
            {
                let turn_checkpoint =
                    checkpoint::capture_turn(project_path, session.id, turn_count).unwrap_or_else(
                        |_| Checkpoint {
                            turn_count,
                            git_ref: checkpoint::checkpoint_ref(session.id, turn_count),
                            status: CheckpointStatus::Error,
                            files: Vec::new(),
                            created_at: unix_time(),
                        },
                    );
                if let Some(turn) = session.turns.last_mut() {
                    turn.checkpoint = Some(turn_checkpoint);
                }
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
        let branch = state
            .selected_project
            .and_then(|project_id| {
                state
                    .projects
                    .iter()
                    .find(|project| project.id == project_id)
            })
            .and_then(|project| git_branch(&project.path));
        let transcript_rows = ListState::new(0, ListAlignment::Bottom, px(512.0)).measure_all();
        let anchored_transcript_rows =
            ListState::new(0, ListAlignment::Top, px(512.0)).measure_all();
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
                    this.reload_clean_right_panel_file_editors(window, cx);
                    if this.settings_page == Some(SettingsPage::ComputerUse) {
                        this.request_computer_permissions(false, cx);
                    }
                }
            })
            .detach();

            cx.subscribe(
                &composer,
                |this: &mut Self, _, event: &ComposerEvent, cx| match event {
                    ComposerEvent::Submit(prompt) => this.submit_prompt(prompt.clone(), cx),
                },
            )
            .detach();

            cx.observe(&composer, |_, _, cx| cx.notify()).detach();
            cx.subscribe(&model_search, |_: &mut Self, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            })
            .detach();
            cx.subscribe(
                &settings_search,
                |_: &mut Self, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
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
                panel_resize_drag: None,
                right_panel_session_states: HashMap::new(),
                right_panel_surfaces: Vec::new(),
                right_panel_active_surface: None,
                right_panel_tabs_scroll_handle: ScrollHandle::new(),
                right_panel_pending_tab_reveal: None,
                right_panel_pending_terminal_focus: None,
                right_panel_expanded_paths: HashSet::new(),
                right_panel_files_selected_path: None,
                right_panel_file_tree_width: DEFAULT_FILE_TREE_WIDTH,
                right_panel_file_editors: HashMap::new(),
                right_panel_diff_files: Vec::new(),
                right_panel_terminals: HashMap::new(),
                settings_page: None,
                header_drag_armed: false,
                branch,
                toast: None,
                copied_message_feedback: HashMap::new(),
                copied_message_generation: 0,
                copied_activity_feedback: HashMap::new(),
                copied_activity_generation: 0,
                message_edit: None,
                transcript_rows,
                anchored_transcript_rows,
                transcript_row_kinds: RefCell::new(Vec::new()),
                transcript_anchor: Cell::new(None),
                transcript_anchor_end_space: Rc::new(Cell::new(Pixels::ZERO)),
                transcript_anchor_following,
                transcript_is_scrolled,
                transcript_layout_width: Cell::new(Pixels::ZERO),
                message_text_states: HashMap::new(),
                activity_text_states: RefCell::new(HashMap::new()),
                navigation_rail: navigation_rail.clone(),
                navigation_rail_active_scale_enabled,
                navigation_rail_reset_generation: Cell::new(0),
            }
        });
        navigation_rail.update(cx, |rail, _| rail.set_waku(entity.downgrade()));
        let initial_row_count = entity.read(cx).transcript_row_count();
        entity.read(cx).reset_transcript_rows(initial_row_count);
        entity
    }
}

#[cfg(test)]
mod tests;
