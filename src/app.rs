use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, unbounded};
use gpui::{
    Animation, AnimationExt, AnyElement, App, ClipboardItem, Context, Corner, Div, Entity,
    FocusHandle, Focusable, FontWeight, Hsla, IntoElement, ListAlignment, ListOffset, ListState,
    MouseButton, PathPromptOptions, Pixels, Point, Render, SharedString, Size, Stateful,
    StyleRefinement, Timer, Window, div, list, point, prelude::*, pulsating_between, px, rems,
    size,
};
use uuid::Uuid;

use crate::checkpoint;
use crate::driver::{self, DriverHandle, DriverStartOptions};
use crate::input::{ComposerEvent, ComposerInput, preserve_composer_focus_for_context_menu};
use crate::model::{
    ActivityItem, AgentSession, Checkpoint, CheckpointStatus, DriverEvent, FavoriteModel,
    InteractionMode, Message, MessageRole, PendingPermission, Project, ProviderKind, ProviderModel,
    ProviderProbe, ReasoningBlock, RuntimeMode, SessionStatus, TranscriptBlock,
    TranscriptBlockContent, TurnStatus, compact_path, unix_time, unix_time_millis,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::popover::Popover;
use gpui_component::scroll::{ScrollableElement, ScrollbarHandle};
use gpui_component::text::{
    TextView, TextViewBlockResize, TextViewScrollViewport, TextViewState, TextViewStyle,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::persistence::{PersistedState, StateStore};
use crate::theme::{Theme, ThemePreference};
use crate::ui::{
    MenuChip, activity_icon, activity_noun, icon, provider_color, provider_icon, status_color,
    status_label,
};
use crate::{CancelTurn, CloseWindow, FocusComposer, NewSession, OpenSettings, ToggleSidebar};

const TRAFFIC_LIGHT_CLEARANCE: f32 = 86.0;
const CONTENT_MAX_WIDTH: f32 = 720.0;
const SIDEBAR_WIDTH: f32 = 252.0;
const FOLLOWUP_TURN_TOP_GAP: f32 = 48.0;
const STREAM_FRAME_INTERVAL: Duration = Duration::from_millis(24);
const STREAM_MARKDOWN_DELAY: Duration = Duration::from_millis(32);
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
    Appearance,
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
struct CheckpointAction {
    session_id: Uuid,
    turn_count: usize,
    file_count: usize,
    can_revert: bool,
    confirmed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TranscriptAnchor {
    session_id: Uuid,
    turn_id: Uuid,
}

/// Presents a stable, estimated document length to the scrollbar while the
/// virtualized list replaces provisional row heights with exact measurements.
/// Offsets are normalized against the list's live range, so the thumb remains
/// anchored at the same logical position and dragging still reaches both ends.
#[derive(Clone)]
struct StableListScrollbarHandle {
    list_state: ListState,
    estimated_content_height: Rc<Cell<Pixels>>,
    anchor_end_space: Rc<Cell<Pixels>>,
    anchor_following: Rc<Cell<bool>>,
    drag_estimated_height: Rc<Cell<Option<Pixels>>>,
    is_scrolled: Rc<Cell<bool>>,
}

impl StableListScrollbarHandle {
    fn new(
        list_state: &ListState,
        estimated_content_height: &Rc<Cell<Pixels>>,
        anchor_end_space: &Rc<Cell<Pixels>>,
        anchor_following: &Rc<Cell<bool>>,
        drag_estimated_height: &Rc<Cell<Option<Pixels>>>,
        is_scrolled: &Rc<Cell<bool>>,
    ) -> Self {
        Self {
            list_state: list_state.clone(),
            estimated_content_height: estimated_content_height.clone(),
            anchor_end_space: anchor_end_space.clone(),
            anchor_following: anchor_following.clone(),
            drag_estimated_height: drag_estimated_height.clone(),
            is_scrolled: is_scrolled.clone(),
        }
    }

    fn effective_content_height(&self) -> Pixels {
        self.drag_estimated_height
            .get()
            .unwrap_or_else(|| self.estimated_content_height.get() + self.anchor_end_space.get())
    }

    fn actual_max_offset(&self) -> Size<Pixels> {
        let viewport = self.list_state.viewport_bounds().size;
        let base = self.list_state.max_offset_for_scrollbar();
        let estimated_max = (self.effective_content_height() - viewport.height).max(Pixels::ZERO);
        size(
            base.width,
            if base.height > Pixels::ZERO {
                base.height + self.anchor_end_space.get()
            } else {
                estimated_max
            },
        )
    }
}

fn scale_scrollbar_offset(
    offset: Point<Pixels>,
    source_max: Size<Pixels>,
    target_max: Size<Pixels>,
) -> Point<Pixels> {
    let scale_axis = |offset: Pixels, source: Pixels, target: Pixels| {
        if source <= Pixels::ZERO || target <= Pixels::ZERO {
            Pixels::ZERO
        } else {
            (offset / source * target).clamp(-target, Pixels::ZERO)
        }
    };
    point(
        scale_axis(offset.x, source_max.width, target_max.width),
        scale_axis(offset.y, source_max.height, target_max.height),
    )
}

fn scroll_top_after_row_invalidation(
    mut scroll_top: ListOffset,
    range: Range<usize>,
    anchor_delta: Pixels,
) -> Option<ListOffset> {
    if !range.contains(&scroll_top.item_ix) {
        return None;
    }
    if scroll_top.item_ix == range.start {
        scroll_top.offset_in_item = (scroll_top.offset_in_item + anchor_delta).max(Pixels::ZERO);
    }
    Some(scroll_top)
}

impl ScrollbarHandle for StableListScrollbarHandle {
    fn offset(&self) -> Point<Pixels> {
        let viewport = self.list_state.viewport_bounds().size;
        let actual_max = self.actual_max_offset();
        let estimated_max = size(
            Pixels::ZERO,
            (self.effective_content_height() - viewport.height).max(Pixels::ZERO),
        );
        scale_scrollbar_offset(
            self.list_state.scroll_px_offset_for_scrollbar(),
            actual_max,
            estimated_max,
        )
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        self.anchor_following.set(false);
        let viewport = self.list_state.viewport_bounds().size;
        let actual_max = self.actual_max_offset();
        let estimated_max = size(
            Pixels::ZERO,
            (self.effective_content_height() - viewport.height).max(Pixels::ZERO),
        );
        let actual_offset = scale_scrollbar_offset(offset, estimated_max, actual_max);
        self.list_state.set_offset_from_scrollbar(actual_offset);
        let at_end = estimated_max.height <= Pixels::ZERO
            || -offset.y >= (estimated_max.height - px(0.5)).max(Pixels::ZERO);
        self.is_scrolled.set(!at_end);
    }

    fn content_size(&self) -> Size<Pixels> {
        let viewport = self.list_state.viewport_bounds().size;
        size(
            viewport.width,
            self.effective_content_height().max(viewport.height),
        )
    }

    fn start_drag(&self) {
        self.drag_estimated_height.set(Some(
            self.estimated_content_height.get() + self.anchor_end_space.get(),
        ));
        self.list_state.scrollbar_drag_started();
    }

    fn end_drag(&self) {
        self.list_state.scrollbar_drag_ended();
        self.drag_estimated_height.set(None);
    }
}

#[derive(Clone, Copy, Debug)]
struct TranscriptMarkdownResize {
    session_id: Uuid,
    message_id: Uuid,
    delta: Pixels,
    anchor_delta: Pixels,
}

struct SessionRuntime {
    driver: DriverHandle,
    events: Receiver<DriverEvent>,
    pending_events: VecDeque<DriverEvent>,
    stream_phase: Option<StreamPhase>,
    stream_remeasure_pending: bool,
    pending_permission: Option<PendingPermission>,
}

pub struct Waku {
    state: PersistedState,
    store: StateStore,
    composer: Entity<ComposerInput>,
    model_search: Entity<InputState>,
    settings_search: Entity<InputState>,
    probes: Vec<ProviderProbe>,
    provider_probe_events: Receiver<ProviderProbe>,
    model_picker_tab: ModelPickerTab,
    runtimes: HashMap<Uuid, SessionRuntime>,
    stream_state_dirty: bool,
    last_stream_save: Instant,
    /// User expansion overrides keyed by persisted transcript block index.
    reasoning_expanded: HashMap<usize, bool>,
    activities_expanded: HashMap<usize, bool>,
    /// Individual tool rows the user has opened to read their full detail.
    expanded_activity_items: HashSet<Uuid>,
    sidebar_visible: bool,
    settings_page: Option<SettingsPage>,
    header_drag_armed: bool,
    branch: Option<String>,
    toast: Option<String>,
    pending_revert: Option<(Uuid, usize)>,
    transcript_rows: ListState,
    /// Active turns use top alignment so row remeasurement cannot invoke the
    /// bottom-aligned list's implicit pin and displace the sent-message anchor.
    anchored_transcript_rows: ListState,
    transcript_row_kinds: RefCell<Vec<TranscriptRowKind>>,
    transcript_row_estimates: RefCell<Vec<Pixels>>,
    transcript_row_height_adjustments: RefCell<HashMap<TranscriptRowKind, Pixels>>,
    transcript_estimated_height: Rc<Cell<Pixels>>,
    transcript_anchor: Cell<Option<TranscriptAnchor>>,
    transcript_anchor_end_space: Rc<Cell<Pixels>>,
    transcript_anchor_following: Rc<Cell<bool>>,
    transcript_drag_estimated_height: Rc<Cell<Option<Pixels>>>,
    transcript_provisional_rows: RefCell<HashSet<usize>>,
    transcript_exact_measurement_rows: RefCell<HashSet<usize>>,
    transcript_is_scrolled: Rc<Cell<bool>>,
    transcript_layout_width: Cell<Pixels>,
    transcript_resize_tx: crossbeam_channel::Sender<TranscriptMarkdownResize>,
    transcript_resize_rx: Receiver<TranscriptMarkdownResize>,
    message_text_states: HashMap<Uuid, Entity<TextViewState>>,
}

mod components;
mod composer;
mod render;
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
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let store = StateStore::new(StateStore::default_path());
        let mut state = store.load_or_fresh(cwd);
        crate::theme::apply_theme_preference(state.theme, window, cx);
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
        for provider in ProviderKind::ALL {
            let provider_probe_tx = provider_probe_tx.clone();
            let _ = std::thread::Builder::new()
                .name(format!("waku-{}-probe", provider.id()))
                .spawn(move || {
                    let _ = provider_probe_tx.send(ProviderProbe::detect(provider));
                });
        }
        drop(provider_probe_tx);
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
        transcript_rows.set_scroll_handler({
            let transcript_is_scrolled = transcript_is_scrolled.clone();
            let transcript_anchor_following = transcript_anchor_following.clone();
            move |event, _, _| {
                transcript_is_scrolled.set(event.is_scrolled);
                transcript_anchor_following.set(false);
            }
        });
        anchored_transcript_rows.set_scroll_handler({
            let transcript_is_scrolled = transcript_is_scrolled.clone();
            let transcript_anchor_following = transcript_anchor_following.clone();
            move |event, _, _| {
                transcript_is_scrolled.set(event.is_scrolled);
                transcript_anchor_following.set(false);
            }
        });
        let (transcript_resize_tx, transcript_resize_rx) = unbounded();

        let entity = cx.new(|cx| {
            cx.observe_window_appearance(window, |this: &mut Self, window, cx| {
                if this.state.theme == ThemePreference::System {
                    crate::theme::apply_theme_preference(this.state.theme, window, cx);
                    cx.notify();
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
                    Timer::after(STREAM_FRAME_INTERVAL).await;
                    if this
                        .update(cx, |this, cx| {
                            if this.drain_driver_events() || this.drain_provider_probe_events() {
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
                probes,
                provider_probe_events,
                model_picker_tab,
                runtimes: HashMap::new(),
                stream_state_dirty: false,
                last_stream_save: Instant::now(),
                reasoning_expanded: HashMap::new(),
                activities_expanded: HashMap::new(),
                expanded_activity_items: HashSet::new(),
                sidebar_visible: true,
                settings_page: None,
                header_drag_armed: false,
                branch,
                toast: None,
                pending_revert: None,
                transcript_rows,
                anchored_transcript_rows,
                transcript_row_kinds: RefCell::new(Vec::new()),
                transcript_row_estimates: RefCell::new(Vec::new()),
                transcript_row_height_adjustments: RefCell::new(HashMap::new()),
                transcript_estimated_height: Rc::new(Cell::new(Pixels::ZERO)),
                transcript_anchor: Cell::new(None),
                transcript_anchor_end_space: Rc::new(Cell::new(Pixels::ZERO)),
                transcript_anchor_following,
                transcript_drag_estimated_height: Rc::new(Cell::new(None)),
                transcript_provisional_rows: RefCell::new(HashSet::new()),
                transcript_exact_measurement_rows: RefCell::new(HashSet::new()),
                transcript_is_scrolled,
                transcript_layout_width: Cell::new(Pixels::ZERO),
                transcript_resize_tx,
                transcript_resize_rx,
                message_text_states: HashMap::new(),
            }
        });
        let initial_row_count = entity.read(cx).transcript_row_count();
        entity.read(cx).reset_transcript_rows(initial_row_count);
        entity
    }
}

#[cfg(test)]
mod tests;
