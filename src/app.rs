use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, unbounded};
use gpui::{
    Animation, AnimationExt, AnyElement, App, BoxShadow, ClipboardItem, Context, Corner, Div,
    Entity, FocusHandle, Focusable, FontWeight, Hsla, IntoElement, ListAlignment, ListOffset,
    ListState, MouseButton, PathPromptOptions, Pixels, Point, Render, SharedString, Size,
    StyleRefinement, Timer, Window, div, hsla, list, point, prelude::*, pulsating_between, px,
    rems, size,
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
use crate::theme::Theme;
use crate::ui::{
    MenuChip, activity_icon, activity_noun, icon, key_hint, provider_color, provider_icon,
    relative_time, section_label, status_color, status_label,
};
use crate::{CancelTurn, FocusComposer, NewSession, ToggleSidebar};

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

impl Waku {
    pub fn new(window: &mut Window, cx: &mut App) -> Entity<Self> {
        let composer = cx.new(|cx| ComposerInput::new(window, cx));
        let model_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search models..."));
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let store = StateStore::new(StateStore::default_path());
        let mut state = store.load_or_fresh(cwd);
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

    pub fn composer_focus(&self, cx: &App) -> FocusHandle {
        self.composer.read(cx).focus()
    }

    fn selected_project(&self) -> Option<&Project> {
        let id = self.state.selected_project?;
        self.state.projects.iter().find(|project| project.id == id)
    }

    fn selected_session(&self) -> Option<&AgentSession> {
        let id = self.state.selected_session?;
        self.state.sessions.iter().find(|session| session.id == id)
    }

    fn selected_session_mut(&mut self) -> Option<&mut AgentSession> {
        let id = self.state.selected_session?;
        self.state
            .sessions
            .iter_mut()
            .find(|session| session.id == id)
    }

    fn selected_runtime(&self) -> Option<&SessionRuntime> {
        self.runtimes.get(&self.state.selected_session?)
    }

    fn provider_probe(&self, provider: ProviderKind) -> Option<&ProviderProbe> {
        self.probes.iter().find(|probe| probe.provider == provider)
    }

    fn model_for_session<'a>(&'a self, session: &'a AgentSession) -> Option<&'a str> {
        session.model.as_deref().or_else(|| {
            self.provider_probe(session.provider)
                .and_then(ProviderProbe::preferred_model)
                .map(|model| model.id.as_str())
        })
    }

    fn model_display_name(&self, provider: ProviderKind, model: Option<&str>) -> String {
        let Some(model) = model else {
            return provider.short_name().to_owned();
        };
        self.provider_probe(provider)
            .and_then(|probe| probe.models.iter().find(|candidate| candidate.id == model))
            .map(|candidate| candidate.name.clone())
            .unwrap_or_else(|| model.to_owned())
    }

    fn model_metadata_for_session(&self, session: &AgentSession) -> Option<&ProviderModel> {
        let model = self.model_for_session(session)?;
        self.provider_probe(session.provider)?
            .models
            .iter()
            .find(|candidate| candidate.id == model)
    }

    fn selected_transcript_blocks(&self) -> &[TranscriptBlock] {
        self.selected_session()
            .map(|session| session.transcript_blocks.as_slice())
            .unwrap_or(&[])
    }

    fn save(&mut self) {
        self.last_stream_save = Instant::now();
        if let Err(error) = self.store.save(&self.state) {
            self.toast = Some(format!("Could not save local state: {error}"));
        } else {
            self.stream_state_dirty = false;
        }
    }

    fn capture_latest_turn_checkpoint(&mut self) {
        if let Some(session_id) = self.state.selected_session {
            self.capture_latest_turn_checkpoint_for(session_id);
        }
    }

    fn capture_latest_turn_checkpoint_for(&mut self, session_id: Uuid) {
        let Some((project_id, turn_count)) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| {
                session
                    .turns
                    .last()
                    .filter(|turn| turn.status != TurnStatus::Running)
                    .map(|turn| (session.project_id, turn.turn_count))
            })
        else {
            return;
        };
        let Some(project_path) = self
            .state
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .map(|project| project.path.clone())
        else {
            return;
        };

        let checkpoint = match checkpoint::capture_turn(&project_path, session_id, turn_count) {
            Ok(checkpoint) => checkpoint,
            Err(error) => {
                self.toast = Some(format!("Could not capture the turn checkpoint: {error}"));
                Checkpoint {
                    turn_count,
                    git_ref: checkpoint::checkpoint_ref(session_id, turn_count),
                    status: CheckpointStatus::Error,
                    files: Vec::new(),
                    created_at: unix_time(),
                }
            }
        };
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            && let Some(turn) = session
                .turns
                .iter_mut()
                .find(|turn| turn.turn_count == turn_count)
        {
            turn.checkpoint = Some(checkpoint);
        }
    }

    fn request_checkpoint_revert(
        &mut self,
        session_id: Uuid,
        turn_count: usize,
        cx: &mut Context<Self>,
    ) {
        if self.pending_revert == Some((session_id, turn_count)) {
            self.pending_revert = None;
            self.revert_to_checkpoint(session_id, turn_count, cx);
            return;
        }

        let discarded_turns = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .map(|session| session.turns.len().saturating_sub(turn_count))
            .unwrap_or_default();
        self.pending_revert = Some((session_id, turn_count));
        self.toast = Some(if discarded_turns == 0 {
            "Click “Confirm revert” to restore the workspace to this checkpoint.".into()
        } else {
            format!(
                "Click “Confirm revert” to restore the workspace and discard {discarded_turns} later turn(s)."
            )
        });
        cx.notify();
    }

    fn revert_to_checkpoint(
        &mut self,
        session_id: Uuid,
        turn_count: usize,
        cx: &mut Context<Self>,
    ) {
        let Some((
            project_id,
            provider,
            status,
            provider_cursor,
            previous_turn_count,
            rollback_turns,
            checkpoint,
        )) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| {
                session
                    .turns
                    .iter()
                    .find(|turn| turn.turn_count == turn_count)
                    .and_then(|turn| turn.checkpoint.clone())
                    .map(|checkpoint| {
                        (
                            session.project_id,
                            session.provider,
                            session.status,
                            session.provider_cursor.clone(),
                            session.turns.len(),
                            session.provider_turns_after(turn_count),
                            checkpoint,
                        )
                    })
            })
        else {
            self.toast = Some("That checkpoint is no longer available.".into());
            cx.notify();
            return;
        };
        if self.state.selected_session != Some(session_id) {
            self.toast = Some("Select the task before reverting its checkpoint.".into());
            cx.notify();
            return;
        }
        if status != SessionStatus::Idle {
            self.toast = Some("Stop the current turn before reverting a checkpoint.".into());
            cx.notify();
            return;
        }
        if checkpoint.status != CheckpointStatus::Ready {
            self.toast = Some("This turn does not have a restorable Git checkpoint.".into());
            cx.notify();
            return;
        }
        if !provider.supports_conversation_rollback() || provider_cursor.is_none() {
            self.toast = Some(format!(
                "{} cannot safely roll back its native conversation yet.",
                provider.display_name()
            ));
            cx.notify();
            return;
        }
        let Some(project_path) = self
            .state
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .map(|project| project.path.clone())
        else {
            self.toast = Some("The task's project could not be found.".into());
            cx.notify();
            return;
        };
        if !checkpoint::has_ref(&project_path, &checkpoint.git_ref) {
            self.toast = Some("The checkpoint's hidden Git ref is missing.".into());
            cx.notify();
            return;
        }

        let safety_ref = format!("refs/waku/revert-backup-{session_id}-{}", Uuid::new_v4());
        if let Err(error) = checkpoint::capture_ref(&project_path, &safety_ref) {
            self.toast = Some(format!(
                "Could not create a revert safety snapshot: {error}"
            ));
            cx.notify();
            return;
        }
        if let Err(error) = checkpoint::restore_ref(&project_path, &checkpoint.git_ref) {
            self.toast = Some(match checkpoint::restore_ref(&project_path, &safety_ref) {
                Ok(()) => {
                    let _ = checkpoint::delete_ref(&project_path, &safety_ref);
                    format!("Could not restore the checkpoint: {error}")
                }
                Err(restore_error) => format!(
                    "Checkpoint restore failed ({error}); safety restore also failed ({restore_error}). Recovery ref retained at {safety_ref}."
                ),
            });
            cx.notify();
            return;
        }

        if rollback_turns > 0 {
            let rollback_result = self
                .ensure_driver()
                .and_then(|driver| driver.rollback(rollback_turns));
            if let Err(error) = rollback_result {
                let restore_result = checkpoint::restore_ref(&project_path, &safety_ref);
                self.toast = Some(match restore_result {
                    Ok(()) => {
                        let _ = checkpoint::delete_ref(&project_path, &safety_ref);
                        format!(
                            "The provider rejected the rollback, so the workspace was restored: {error}"
                        )
                    }
                    Err(restore_error) => format!(
                        "Provider rollback failed ({error}) and the safety snapshot could not be restored ({restore_error}). Recovery ref retained at {safety_ref}."
                    ),
                });
                cx.notify();
                return;
            }
        }

        let _ = checkpoint::delete_ref(&project_path, &safety_ref);
        let cleanup_result = checkpoint::delete_turn_refs_after(
            &project_path,
            session_id,
            turn_count,
            previous_turn_count,
        );
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.truncate_after_turn(turn_count);
            session.status = SessionStatus::Idle;
        }
        if let Some(runtime) = self.runtimes.get_mut(&session_id) {
            runtime.pending_events.clear();
            runtime.stream_remeasure_pending = false;
            runtime.stream_phase = None;
            runtime.pending_permission = None;
        }
        self.reasoning_expanded.clear();
        self.activities_expanded.clear();
        self.expanded_activity_items.clear();
        self.reset_transcript_rows(self.transcript_row_count());
        self.toast = Some(match cleanup_result {
            Ok(()) => format!("Restored checkpoint after turn {turn_count}."),
            Err(error) => {
                format!("Restored checkpoint after turn {turn_count}; stale refs remain: {error}")
            }
        });
        self.save();
        cx.notify();
    }

    fn ensure_driver(&mut self) -> anyhow::Result<DriverHandle> {
        let session = self
            .selected_session()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("No session selected"))?;
        if let Some(runtime) = self.runtimes.get(&session.id) {
            return Ok(runtime.driver.clone());
        }
        let project = self
            .state
            .projects
            .iter()
            .find(|project| project.id == session.project_id)
            .ok_or_else(|| anyhow::anyhow!("Project not found"))?;
        let binary = self
            .probes
            .iter()
            .find(|probe| probe.provider == session.provider)
            .and_then(|probe| probe.path.clone())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{} is not installed or could not be found",
                    session.provider.display_name()
                )
            })?;
        let model = session.model.clone().or_else(|| {
            self.probes
                .iter()
                .find(|probe| probe.provider == session.provider)
                .and_then(ProviderProbe::preferred_model)
                .map(|model| model.id.clone())
        });
        let model_metadata = self.model_metadata_for_session(&session);
        let reasoning_effort = session.reasoning_effort.clone().filter(|effort| {
            model_metadata.is_some_and(|model| {
                model
                    .reasoning_efforts
                    .iter()
                    .any(|option| option.id == *effort)
            })
        });
        let service_tier = session.service_tier.clone().filter(|tier| {
            tier == "default"
                || model_metadata.is_some_and(|model| {
                    model.service_tiers.iter().any(|option| option.id == *tier)
                })
        });
        let (event_tx, event_rx) = unbounded();
        let handle = driver::start(
            session.provider,
            DriverStartOptions {
                binary,
                cwd: project.path.clone(),
                mode: session.runtime_mode,
                interaction_mode: session.interaction_mode,
                model,
                reasoning_effort,
                service_tier,
                provider_cursor: session.provider_cursor.clone(),
            },
            event_tx,
        )?;
        self.runtimes.insert(
            session.id,
            SessionRuntime {
                driver: handle.clone(),
                events: event_rx,
                pending_events: VecDeque::new(),
                stream_phase: None,
                stream_remeasure_pending: false,
                pending_permission: None,
            },
        );
        Ok(handle)
    }

    fn submit_prompt(&mut self, prompt: String, cx: &mut Context<Self>) {
        let Some((session_id, project_id, status, next_turn_count)) =
            self.selected_session().map(|session| {
                (
                    session.id,
                    session.project_id,
                    session.status,
                    session.turns.len() + 1,
                )
            })
        else {
            return;
        };
        if matches!(
            status,
            SessionStatus::Working | SessionStatus::Connecting | SessionStatus::Waiting
        ) {
            self.toast = Some("The agent is already working. Stop it before sending again.".into());
            cx.notify();
            return;
        }
        let project_path = self
            .state
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .map(|project| project.path.clone());
        let checkpoint_warning = project_path.as_deref().and_then(|path| {
            let baseline_count = next_turn_count - 1;
            let git_ref = checkpoint::checkpoint_ref(session_id, baseline_count);
            (!checkpoint::has_ref(path, &git_ref))
                .then(|| checkpoint::capture_turn(path, session_id, baseline_count).err())
                .flatten()
                .map(|error| format!("Could not capture the pre-turn checkpoint: {error}"))
        });
        let transcript_anchor = if let Some(session) = self.selected_session_mut() {
            session.set_title_from_prompt(&prompt);
            let turn_id = session.begin_turn(&prompt);
            session.status = SessionStatus::Connecting;
            session.updated_at = unix_time();
            Some(TranscriptAnchor {
                session_id,
                turn_id,
            })
        } else {
            None
        };
        if let Some(runtime) = self.runtimes.get_mut(&session_id) {
            runtime.pending_events.clear();
            runtime.stream_remeasure_pending = false;
            runtime.stream_phase = None;
            runtime.pending_permission = None;
        }
        self.reasoning_expanded.clear();
        self.activities_expanded.clear();
        self.expanded_activity_items.clear();
        self.pending_revert = None;
        self.toast = checkpoint_warning;
        self.transcript_anchor.set(transcript_anchor);
        self.transcript_anchor_end_space.set(Pixels::ZERO);
        self.transcript_anchor_following.set(true);
        self.reset_transcript_rows(self.transcript_row_count());
        self.scroll_transcript_to_anchor();
        let mut failed_to_start = false;
        match self.ensure_driver() {
            Ok(driver) => driver.prompt(prompt),
            Err(error) => {
                failed_to_start = true;
                let message = format!("Could not start the agent: {error}");
                if let Some(session) = self.selected_session_mut() {
                    session.status = SessionStatus::Failed;
                    session.push_message(MessageRole::Assistant, message);
                    session.finish_active_turn(TurnStatus::Failed);
                }
            }
        }
        if failed_to_start {
            self.capture_latest_turn_checkpoint();
        }
        self.save();
        cx.notify();
    }

    fn collect_runtime_events(runtime: &mut SessionRuntime) {
        while let Ok(event) = runtime.events.try_recv() {
            runtime.pending_events.push_back(event);
        }
    }

    fn drain_provider_probe_events(&mut self) -> bool {
        let mut changed = false;
        while let Ok(probe) = self.provider_probe_events.try_recv() {
            if let Some(existing) = self
                .probes
                .iter_mut()
                .find(|existing| existing.provider == probe.provider)
            {
                *existing = probe;
            } else {
                self.probes.push(probe);
            }
            changed = true;
        }
        changed
    }

    fn drain_driver_events(&mut self) -> bool {
        let session_ids = self.runtimes.keys().copied().collect::<Vec<_>>();
        let mut changed = false;
        let mut force_save = false;
        let mut selected_changed = false;
        for session_id in session_ids {
            let Some(mut runtime) = self.runtimes.remove(&session_id) else {
                continue;
            };
            let follow_up_remeasure = std::mem::take(&mut runtime.stream_remeasure_pending);
            Self::collect_runtime_events(&mut runtime);
            let mut runtime_changed = false;
            let mut markdown_changed = false;
            let mut revealed_stream_chunk = false;
            let mut keep_runtime = true;
            while let Some(event) = runtime.pending_events.front() {
                let kind = stream_delta_kind(event);
                if kind.is_some() && revealed_stream_chunk {
                    break;
                }

                let event = if let Some(kind) = kind {
                    revealed_stream_chunk = true;
                    pop_stream_chunk(&mut runtime.pending_events, kind)
                } else {
                    runtime.pending_events.pop_front()
                };
                let Some(event) = event else {
                    break;
                };
                force_save |= matches!(
                    event,
                    DriverEvent::Connected { .. }
                        | DriverEvent::Permission { .. }
                        | DriverEvent::TurnFinished { .. }
                        | DriverEvent::Error(_)
                        | DriverEvent::ProcessExited
                );
                markdown_changed |= matches!(event, DriverEvent::TextDelta(_));
                runtime_changed = true;
                keep_runtime &= self.handle_driver_event(session_id, &mut runtime, event);
                if !keep_runtime {
                    break;
                }
            }
            runtime.stream_remeasure_pending = markdown_changed;
            if keep_runtime {
                self.runtimes.insert(session_id, runtime);
            }
            changed |= runtime_changed;
            if self.state.selected_session == Some(session_id)
                && (runtime_changed || follow_up_remeasure)
            {
                selected_changed = true;
            }
        }

        if changed {
            self.stream_state_dirty = true;
        }
        if selected_changed {
            self.remeasure_transcript_tail();
        }
        if self.stream_state_dirty
            && (force_save || self.last_stream_save.elapsed() >= STREAM_SAVE_INTERVAL)
        {
            self.save();
        }
        changed || selected_changed
    }

    /// One list row per message plus each ordered non-message turn block.
    fn transcript_row_count(&self) -> usize {
        let messages = self
            .selected_session()
            .map(|session| session.messages.len())
            .unwrap_or(0);
        messages + self.selected_transcript_blocks().len()
    }

    fn selected_transcript_row_kinds(&self) -> Vec<TranscriptRowKind> {
        let message_count = self
            .selected_session()
            .map(|session| session.messages.len())
            .unwrap_or(0);
        let anchors = self
            .selected_transcript_blocks()
            .iter()
            .map(|block| block.after_message)
            .collect::<Vec<_>>();
        transcript_row_kinds(message_count, &anchors)
    }

    fn estimated_transcript_row_height(
        &self,
        row_index: usize,
        kind: TranscriptRowKind,
        row_count: usize,
    ) -> Pixels {
        let inner_height = match kind {
            TranscriptRowKind::Message(message_index) => self
                .selected_session()
                .and_then(|session| session.messages.get(message_index))
                .map(|message| {
                    estimated_message_height(
                        message,
                        self.transcript_layout_width.get(),
                        self.checkpoint_action_for_message(message_index).is_some(),
                    )
                })
                .unwrap_or(px(36.0)),
            TranscriptRowKind::TurnBlock(block_index) => self
                .selected_transcript_blocks()
                .get(block_index)
                .map(|block| match &block.content {
                    TranscriptBlockContent::Reasoning(reasoning) => {
                        let live = self.reasoning_live()
                            && self.selected_transcript_blocks().iter().rposition(|block| {
                                matches!(block.content, TranscriptBlockContent::Reasoning(_))
                            }) == Some(block_index);
                        let expanded = self
                            .reasoning_expanded
                            .get(&block_index)
                            .copied()
                            .unwrap_or(live);
                        px(22.0)
                            + if expanded {
                                px(6.0) + estimated_text_height(&reasoning.content, 88, 18.0)
                            } else {
                                Pixels::ZERO
                            }
                    }
                    TranscriptBlockContent::Activities(activities) => {
                        let running = activities.iter().any(|activity| !activity.complete);
                        let expanded = self
                            .activities_expanded
                            .get(&block_index)
                            .copied()
                            .unwrap_or(running);
                        if !expanded {
                            px(22.0)
                        } else {
                            activities.iter().fold(px(22.0), |height, activity| {
                                let detail_height = activity
                                    .detail
                                    .as_deref()
                                    .filter(|detail| {
                                        self.expanded_activity_items.contains(&activity.id)
                                            && !detail.trim().is_empty()
                                    })
                                    .map_or(Pixels::ZERO, |detail| {
                                        px(14.0) + estimated_text_height(detail, 76, 17.0)
                                    });
                                height + px(24.0) + detail_height
                            })
                        }
                    }
                })
                .unwrap_or(px(22.0)),
        };
        let starts_followup_turn = matches!(kind, TranscriptRowKind::Message(message_index)
            if message_index > 0
                && self.selected_session().and_then(|session| session.messages.get(message_index)).is_some_and(|message| message.role == MessageRole::User));
        inner_height
            + estimated_transcript_row_padding(row_index, row_count, starts_followup_turn)
            + self
                .transcript_row_height_adjustments
                .borrow()
                .get(&kind)
                .copied()
                .unwrap_or(Pixels::ZERO)
    }

    fn rebuild_transcript_estimates(&self) {
        let kinds = self.selected_transcript_row_kinds();
        let valid_kinds = kinds.iter().copied().collect::<HashSet<_>>();
        self.transcript_row_height_adjustments
            .borrow_mut()
            .retain(|kind, _| valid_kinds.contains(kind));
        let row_count = kinds.len();
        let estimates = kinds
            .iter()
            .copied()
            .enumerate()
            .map(|(index, kind)| self.estimated_transcript_row_height(index, kind, row_count))
            .collect::<Vec<_>>();
        let total = estimates
            .iter()
            .copied()
            .fold(Pixels::ZERO, |total, height| total + height);
        *self.transcript_row_kinds.borrow_mut() = kinds;
        *self.transcript_row_estimates.borrow_mut() = estimates;
        self.transcript_estimated_height.set(total);
    }

    fn append_transcript_estimates(&self, current: usize, count: usize) -> bool {
        let kinds = self.selected_transcript_row_kinds();
        if kinds.len() != count {
            return false;
        }
        {
            let previous = self.transcript_row_kinds.borrow();
            if previous.len() != current || previous.as_slice() != &kinds[..current] {
                return false;
            }
        }

        let mut estimates = self.transcript_row_estimates.borrow_mut();
        if estimates.len() != current {
            return false;
        }
        let mut total = self.transcript_estimated_height.get();

        // The old tail loses its special bottom padding once another row is
        // appended. Recompute that one row, then estimate only the new tail.
        if current > 0 {
            let index = current - 1;
            let next = self.estimated_transcript_row_height(index, kinds[index], count);
            total += next - estimates[index];
            estimates[index] = next;
        }
        for (index, kind) in kinds.iter().copied().enumerate().skip(current) {
            let height = self.estimated_transcript_row_height(index, kind, count);
            estimates.push(height);
            total += height;
        }

        *self.transcript_row_kinds.borrow_mut() = kinds;
        self.transcript_estimated_height
            .set(total.max(Pixels::ZERO));
        true
    }

    fn update_transcript_estimates(&self, range: Range<usize>) {
        let kinds = self.transcript_row_kinds.borrow();
        if self.transcript_row_estimates.borrow().len() != kinds.len() {
            drop(kinds);
            self.rebuild_transcript_estimates();
            return;
        }

        let mut estimates = self.transcript_row_estimates.borrow_mut();
        let mut total = self.transcript_estimated_height.get();
        for index in range.start.min(kinds.len())..range.end.min(kinds.len()) {
            let next = self.estimated_transcript_row_height(index, kinds[index], kinds.len());
            total += next - estimates[index];
            estimates[index] = next;
        }
        self.transcript_estimated_height
            .set(total.max(Pixels::ZERO));
    }

    fn active_transcript_rows(&self) -> &ListState {
        if self.transcript_anchor.get().is_some() {
            &self.anchored_transcript_rows
        } else {
            &self.transcript_rows
        }
    }

    fn reset_transcript_rows(&self, count: usize) {
        // Row keys contain indices, so dynamic corrections from another
        // session must never bleed into the newly selected transcript.
        self.transcript_row_height_adjustments.borrow_mut().clear();
        self.rebuild_transcript_estimates();
        *self.transcript_provisional_rows.borrow_mut() = (0..count).collect();
        self.transcript_exact_measurement_rows.borrow_mut().clear();
        let _ = self.transcript_rows.clone().measure_all();
        self.transcript_is_scrolled.set(false);
        self.transcript_rows.reset(count);
        let _ = self.anchored_transcript_rows.clone().measure_all();
        self.anchored_transcript_rows.reset(count);
    }

    fn selected_transcript_anchor_row(&self) -> Option<usize> {
        let anchor = self.transcript_anchor.get()?;
        let session = self.selected_session()?;
        if session.id != anchor.session_id {
            return None;
        }
        let message_index = session.messages.iter().position(|message| {
            message.role == MessageRole::User && message.turn_id == Some(anchor.turn_id)
        })?;
        self.transcript_row_kinds
            .borrow()
            .iter()
            .position(|kind| *kind == TranscriptRowKind::Message(message_index))
    }

    fn scroll_transcript_to_anchor(&self) {
        let Some(item_ix) = self.selected_transcript_anchor_row() else {
            return;
        };
        self.active_transcript_rows().scroll_to(ListOffset {
            item_ix,
            offset_in_item: Pixels::ZERO,
        });
        self.transcript_is_scrolled.set(true);
    }

    fn update_transcript_anchor_end_space(&self, window: &Window) -> Pixels {
        let Some(anchor_row) = self.selected_transcript_anchor_row() else {
            self.transcript_anchor_end_space.set(Pixels::ZERO);
            self.transcript_anchor_following.set(false);
            return Pixels::ZERO;
        };

        let viewport_height = {
            let measured = self.active_transcript_rows().viewport_bounds().size.height;
            if measured > Pixels::ZERO {
                measured
            } else {
                // The first sent message replaces the empty state, so the list
                // has no prior bounds yet. The full window is a conservative
                // first-frame fallback that still guarantees a top anchor.
                window.viewport_size().height
            }
        };
        let estimated_tail_height = self
            .transcript_row_estimates
            .borrow()
            .iter()
            .skip(anchor_row)
            .copied()
            .fold(Pixels::ZERO, |height, row| height + row);
        let transcript_rows = self.active_transcript_rows();
        let last_row = transcript_rows.item_count().checked_sub(1);
        let measured_tail_height = last_row.and_then(|last_row| {
            let anchor = transcript_rows.bounds_for_item(anchor_row)?;
            let last = transcript_rows.bounds_for_item(last_row)?;
            Some((last.bottom() - anchor.top()).max(Pixels::ZERO))
        });
        let tail_is_unmeasured = self
            .transcript_provisional_rows
            .borrow()
            .iter()
            .any(|row| *row >= anchor_row)
            || self
                .transcript_exact_measurement_rows
                .borrow()
                .iter()
                .any(|row| *row >= anchor_row);
        let anchored_tail_height = if tail_is_unmeasured {
            // Bounds still describe the element that was just invalidated.
            // The estimate already reflects the requested expanded/collapsed
            // state, so use it to prevent a one-frame underfill on collapse.
            estimated_tail_height
        } else {
            measured_tail_height.unwrap_or(estimated_tail_height)
        };
        let end_space = stabilized_transcript_anchor_end_space(
            viewport_height,
            anchored_tail_height,
            self.transcript_anchor_end_space.get(),
            tail_is_unmeasured,
        );
        self.transcript_anchor_end_space.set(end_space);
        if maintain_transcript_anchor(
            transcript_rows,
            anchor_row,
            self.transcript_anchor_following.get(),
            end_space,
        ) {
            self.transcript_is_scrolled.set(true);
        }
        end_space
    }

    fn invalidate_transcript_rows(&self, range: Range<usize>, anchor_delta: Pixels) {
        let transcript_rows = self.active_transcript_rows();
        let count = transcript_rows.item_count();
        let range = range.start.min(count)..range.end.min(count);
        if range.is_empty() {
            return;
        }

        let preserve_scroll = self.transcript_is_scrolled.get();
        let previous_scroll_top = preserve_scroll.then(|| transcript_rows.logical_scroll_top());
        self.transcript_provisional_rows
            .borrow_mut()
            .extend(range.clone());
        let _ = transcript_rows.clone().measure_all();
        transcript_rows.splice(range.clone(), range.len());

        if let Some(scroll_top) = previous_scroll_top.and_then(|scroll_top| {
            scroll_top_after_row_invalidation(scroll_top, range, anchor_delta)
        }) {
            transcript_rows.scroll_to(scroll_top);
        }
    }

    fn sync_transcript_layout_width(&self, window: &Window) -> bool {
        let sidebar_width = if self.sidebar_visible {
            px(SIDEBAR_WIDTH)
        } else {
            Pixels::ZERO
        };
        let content_width = (window.viewport_size().width - sidebar_width - px(40.0))
            .clamp(px(1.0), px(CONTENT_MAX_WIDTH));
        let previous = self.transcript_layout_width.replace(content_width);
        if previous > Pixels::ZERO && (previous - content_width).abs() < px(1.0) {
            return false;
        }

        // A width change is a real document reflow. Rebase the stable estimate
        // for the new wrap width and let GPUI bulk-measure cheap placeholders
        // before it lays out the visible rows exactly.
        self.rebuild_transcript_estimates();
        let count = self.active_transcript_rows().item_count();
        self.invalidate_transcript_rows(0..count, Pixels::ZERO);
        true
    }

    fn drain_transcript_resize_events(&self) {
        let Some(session_id) = self.state.selected_session else {
            while self.transcript_resize_rx.try_recv().is_ok() {}
            return;
        };
        let mut by_row = HashMap::<usize, (TranscriptRowKind, Pixels, Pixels)>::new();

        while let Ok(event) = self.transcript_resize_rx.try_recv() {
            if event.session_id != session_id {
                continue;
            }
            let Some(message_index) = self.selected_session().and_then(|session| {
                session
                    .messages
                    .iter()
                    .position(|message| message.id == event.message_id)
            }) else {
                continue;
            };
            let kind = TranscriptRowKind::Message(message_index);
            let Some(row_index) = self
                .transcript_row_kinds
                .borrow()
                .iter()
                .position(|candidate| *candidate == kind)
            else {
                continue;
            };
            let entry = by_row
                .entry(row_index)
                .or_insert((kind, Pixels::ZERO, Pixels::ZERO));
            entry.1 += event.delta;
            entry.2 += event.anchor_delta;
        }

        for (row_index, (kind, delta, anchor_delta)) in by_row {
            *self
                .transcript_row_height_adjustments
                .borrow_mut()
                .entry(kind)
                .or_insert(Pixels::ZERO) += delta;
            let row_count = self.transcript_row_kinds.borrow().len();
            let next_estimate = self
                .estimated_transcript_row_height(row_index, kind, row_count)
                .max(px(1.0));
            let applied_delta = {
                let mut estimates = self.transcript_row_estimates.borrow_mut();
                let Some(current) = estimates.get_mut(row_index) else {
                    continue;
                };
                let applied_delta = next_estimate - *current;
                *current = next_estimate;
                applied_delta
            };
            self.transcript_estimated_height
                .set((self.transcript_estimated_height.get() + applied_delta).max(Pixels::ZERO));
            self.invalidate_transcript_rows(row_index..row_index + 1, anchor_delta);
        }
    }

    /// Keep the list's row count in sync with the transcript. Appends keep
    /// the reader's place (or the pinned tail); shrinking resets the view.
    fn sync_transcript_rows(&self) {
        let count = self.transcript_row_count();
        let transcript_rows = self.active_transcript_rows();
        let current = transcript_rows.item_count();
        if count > current {
            if !self.append_transcript_estimates(current, count) {
                self.reset_transcript_rows(count);
                return;
            }
            self.transcript_provisional_rows
                .borrow_mut()
                .extend(current..count);
            let _ = transcript_rows.clone().measure_all();
            transcript_rows.splice(current..current, count - current);
        } else if count < current {
            self.reset_transcript_rows(count);
        }
    }

    /// Provider events arrive in transcript order, so only the active tail can
    /// change height. Keeping earlier row measurements intact is critical
    /// for responsive scrolling while a long answer is still growing.
    fn remeasure_transcript_tail(&self) {
        self.sync_transcript_rows();
        let count = self.active_transcript_rows().item_count();
        let from = count.saturating_sub(STREAM_REMEASURE_TAIL_ROWS);
        if from < count {
            self.update_transcript_estimates(from..count);
            self.invalidate_transcript_rows(from..count, Pixels::ZERO);
        }
    }

    fn remeasure_transcript_block(&self, block_index: usize) {
        self.sync_transcript_rows();
        let row_index = self
            .transcript_row_kinds
            .borrow()
            .iter()
            .position(|kind| *kind == TranscriptRowKind::TurnBlock(block_index));
        if let Some(row_index) = row_index {
            self.update_transcript_estimates(row_index..row_index + 1);
            self.invalidate_transcript_rows(row_index..row_index + 1, Pixels::ZERO);
        }
    }

    fn finish_streaming_assistant(&mut self, session_id: Uuid) {
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            for message in &mut session.messages {
                if message.role == MessageRole::Assistant && message.streaming {
                    message.streaming = false;
                }
            }
        }
    }

    fn append_text_delta(&mut self, session_id: Uuid, runtime: &mut SessionRuntime, delta: String) {
        let continuing = runtime.stream_phase == Some(StreamPhase::Text);
        append_text_delta_to_session(&mut self.state.sessions, session_id, continuing, delta);
        runtime.stream_phase = Some(StreamPhase::Text);
    }

    fn append_reasoning_delta(
        &mut self,
        session_id: Uuid,
        runtime: &mut SessionRuntime,
        delta: String,
    ) {
        let continuing = runtime.stream_phase == Some(StreamPhase::Reasoning);
        if !continuing && delta.trim().is_empty() {
            return;
        }
        let now = unix_time_millis();
        if !continuing {
            self.finish_streaming_assistant(session_id);
        }
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            if continuing
                && let Some(TranscriptBlock {
                    content: TranscriptBlockContent::Reasoning(reasoning),
                    ..
                }) = session.transcript_blocks.last_mut()
            {
                reasoning.content.push_str(&delta);
                reasoning.finished_at_ms = now;
            } else {
                session.transcript_blocks.push(TranscriptBlock {
                    after_message: session.messages.len(),
                    turn_id: session.active_turn_id(),
                    content: TranscriptBlockContent::Reasoning(ReasoningBlock {
                        content: delta,
                        started_at_ms: now,
                        finished_at_ms: now,
                    }),
                });
            }
            session.updated_at = unix_time();
        }
        runtime.stream_phase = Some(StreamPhase::Reasoning);
    }

    fn update_activity(
        &mut self,
        session_id: Uuid,
        runtime: &mut SessionRuntime,
        item: ActivityItem,
    ) {
        if runtime.stream_phase == Some(StreamPhase::Text) {
            self.finish_streaming_assistant(session_id);
        }

        let continuing = runtime.stream_phase == Some(StreamPhase::Activity);
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            for block in session.transcript_blocks.iter_mut().rev() {
                let TranscriptBlockContent::Activities(activities) = &mut block.content else {
                    continue;
                };
                let matching = activities.iter_mut().rev().find(|activity| {
                    item.source_id
                        .as_ref()
                        .is_some_and(|id| activity.source_id.as_ref() == Some(id))
                        || (item.source_id.is_none()
                            && activity.title == item.title
                            && !activity.complete)
                });
                if let Some(activity) = matching {
                    activity.kind = item.kind;
                    activity.title = item.title;
                    activity.complete = item.complete;
                    if item.detail.is_some() {
                        activity.detail = item.detail;
                    }
                    session.updated_at = unix_time();
                    runtime.stream_phase = Some(StreamPhase::Activity);
                    return;
                }
            }

            let after_message = session.messages.len();
            if continuing
                && let Some(TranscriptBlock {
                    after_message: anchor,
                    content: TranscriptBlockContent::Activities(activities),
                    ..
                }) = session.transcript_blocks.last_mut()
                && *anchor == after_message
            {
                activities.push(item);
            } else {
                session.transcript_blocks.push(TranscriptBlock {
                    after_message,
                    turn_id: session.active_turn_id(),
                    content: TranscriptBlockContent::Activities(vec![item]),
                });
            }
            session.updated_at = unix_time();
        }
        runtime.stream_phase = Some(StreamPhase::Activity);
    }

    fn complete_turn_blocks(&mut self, session_id: Uuid) {
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            for block in &mut session.transcript_blocks {
                if let TranscriptBlockContent::Activities(activities) = &mut block.content {
                    for activity in activities {
                        activity.complete = true;
                    }
                }
            }
        }
    }

    fn turn_has_assistant_message(&self, session_id: Uuid) -> bool {
        self.state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .is_some_and(|session| {
                let Some(turn_id) = session.active_turn_id() else {
                    return false;
                };
                session.messages.iter().any(|message| {
                    message.role == MessageRole::Assistant && message.turn_id == Some(turn_id)
                })
            })
    }

    fn accepts_turn_output(&self, session_id: Uuid) -> bool {
        self.state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .is_some_and(|session| {
                session.active_turn_id().is_some()
                    && matches!(
                        session.status,
                        SessionStatus::Connecting | SessionStatus::Working | SessionStatus::Waiting
                    )
            })
    }

    /// Returns whether the runtime should remain attached after this event.
    fn handle_driver_event(
        &mut self,
        session_id: Uuid,
        runtime: &mut SessionRuntime,
        event: DriverEvent,
    ) -> bool {
        match event {
            DriverEvent::Connected { provider_cursor } => {
                if let Some(session) = self
                    .state
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == session_id)
                {
                    session.provider_cursor = provider_cursor;
                    if session.status == SessionStatus::Connecting {
                        session.status = SessionStatus::Working;
                    }
                }
            }
            DriverEvent::TurnStarted => {
                if let Some(session) = self
                    .state
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == session_id)
                    && session.active_turn_id().is_some()
                {
                    session.mark_active_turn_provider_started();
                    session.status = SessionStatus::Working;
                }
            }
            DriverEvent::TextDelta(delta) => {
                if self.accepts_turn_output(session_id) {
                    self.append_text_delta(session_id, runtime, delta);
                }
            }
            DriverEvent::ReasoningDelta(delta) => {
                if self.accepts_turn_output(session_id) {
                    self.append_reasoning_delta(session_id, runtime, delta);
                }
            }
            DriverEvent::Activity {
                id,
                kind,
                title,
                detail,
                complete,
            } => {
                if self.accepts_turn_output(session_id) {
                    self.update_activity(
                        session_id,
                        runtime,
                        ActivityItem::new(id, kind, title, detail, complete),
                    );
                }
            }
            DriverEvent::Permission {
                request_id,
                title,
                detail,
                options,
            } => {
                if self.accepts_turn_output(session_id) {
                    runtime.pending_permission = Some(PendingPermission {
                        request_id,
                        title,
                        detail,
                        options,
                    });
                    if let Some(session) = self
                        .state
                        .sessions
                        .iter_mut()
                        .find(|session| session.id == session_id)
                    {
                        session.status = SessionStatus::Waiting;
                    }
                }
            }
            DriverEvent::TurnFinished { success, summary } => {
                if self
                    .state
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
                    .and_then(AgentSession::active_turn_id)
                    .is_none()
                {
                    return true;
                }
                self.finish_streaming_assistant(session_id);
                self.complete_turn_blocks(session_id);
                runtime.stream_phase = None;
                let needs_fallback = !self.turn_has_assistant_message(session_id);
                if let Some(session) = self
                    .state
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == session_id)
                {
                    session.status = if success {
                        SessionStatus::Idle
                    } else {
                        SessionStatus::Failed
                    };
                    if needs_fallback {
                        session.push_message(
                            MessageRole::Assistant,
                            summary.unwrap_or_else(|| {
                                if success {
                                    "Turn completed.".into()
                                } else {
                                    "The agent stopped before returning a response.".into()
                                }
                            }),
                        );
                    }
                    session.finish_active_turn(if success {
                        TurnStatus::Completed
                    } else {
                        TurnStatus::Failed
                    });
                }
                runtime.pending_permission = None;
                self.capture_latest_turn_checkpoint_for(session_id);
            }
            DriverEvent::Error(error) => {
                if self.state.selected_session == Some(session_id) {
                    self.toast = Some(error.clone());
                }
                let has_active_turn = self
                    .state
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
                    .and_then(AgentSession::active_turn_id)
                    .is_some();
                let should_append = has_active_turn
                    && !self.turn_has_assistant_message(session_id)
                    && self
                        .state
                        .sessions
                        .iter()
                        .find(|session| session.id == session_id)
                        .is_some_and(|session| session.status != SessionStatus::Working);
                if let Some(session) = self
                    .state
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == session_id)
                    && has_active_turn
                {
                    if session.status != SessionStatus::Working {
                        session.status = SessionStatus::Failed;
                    }
                    if should_append {
                        session.push_message(MessageRole::Assistant, error);
                    }
                }
            }
            DriverEvent::ProcessExited => {
                self.finish_streaming_assistant(session_id);
                self.complete_turn_blocks(session_id);
                runtime.stream_phase = None;
                runtime.pending_permission = None;
                let mut finished_turn = false;
                if let Some(session) = self
                    .state
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == session_id)
                    && matches!(
                        session.status,
                        SessionStatus::Connecting | SessionStatus::Working | SessionStatus::Waiting
                    )
                {
                    session.status = SessionStatus::Failed;
                    session.updated_at = unix_time();
                    finished_turn = session.finish_active_turn(TurnStatus::Failed).is_some();
                }
                if finished_turn {
                    self.capture_latest_turn_checkpoint_for(session_id);
                }
                return false;
            }
        }
        true
    }

    fn select_project(&mut self, project_id: Uuid, cx: &mut Context<Self>) {
        self.state.selected_project = Some(project_id);
        let next_session = self
            .state
            .sessions
            .iter()
            .filter(|session| session.project_id == project_id)
            .max_by_key(|session| session.updated_at)
            .map(|session| session.id);
        if let Some(session_id) = next_session {
            self.select_session(session_id, cx);
        } else {
            self.create_session_for(project_id, self.state.last_provider, cx);
        }
    }

    fn select_session(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        self.state.selected_session = Some(session_id);
        if let Some((project_id, provider)) = self
            .selected_session()
            .map(|session| (session.project_id, session.provider))
        {
            self.state.selected_project = Some(project_id);
            self.state.last_provider = provider;
        }
        self.reset_visible_state();
        self.branch = self
            .selected_project()
            .and_then(|project| git_branch(&project.path));
        self.reset_transcript_rows(self.transcript_row_count());
        self.save();
        cx.notify();
    }

    fn create_session_for(
        &mut self,
        project_id: Uuid,
        provider: ProviderKind,
        cx: &mut Context<Self>,
    ) {
        let session = AgentSession::new(project_id, provider);
        let id = session.id;
        self.state.sessions.push(session);
        self.state.selected_project = Some(project_id);
        self.state.selected_session = Some(id);
        self.state.last_provider = provider;
        self.reset_visible_state();
        self.reset_transcript_rows(0);
        self.save();
        cx.notify();
    }

    fn remove_session(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        let Some(index) = self
            .state
            .sessions
            .iter()
            .position(|session| session.id == session_id)
        else {
            return;
        };
        let project_id = self.state.sessions[index].project_id;
        let last_turn_count = self.state.sessions[index].turns.len();
        let project_path = self
            .state
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .map(|project| project.path.clone());
        let was_selected = self.state.selected_session == Some(session_id);
        self.reset_session_runtime(session_id);
        self.state.sessions.remove(index);
        if let Some(project_path) = project_path {
            let _ = checkpoint::delete_session_refs(&project_path, session_id, last_turn_count);
        }

        if !was_selected {
            self.save();
            cx.notify();
            return;
        }

        self.state.selected_session = None;
        let next_session = self
            .state
            .sessions
            .iter()
            .filter(|session| session.project_id == project_id)
            .max_by_key(|session| session.updated_at)
            .map(|session| session.id);
        if let Some(session_id) = next_session {
            self.select_session(session_id, cx);
        } else {
            self.create_session_for(project_id, self.state.last_provider, cx);
        }
    }

    fn new_session_action(&mut self, _: &NewSession, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(project_id) = self.state.selected_project {
            self.create_session_for(project_id, self.state.last_provider, cx);
        }
    }

    fn toggle_sidebar_action(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_visible = !self.sidebar_visible;
        cx.notify();
    }

    fn focus_composer_action(
        &mut self,
        _: &FocusComposer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.composer_focus(cx));
    }

    fn cancel_turn_action(&mut self, _: &CancelTurn, _: &mut Window, cx: &mut Context<Self>) {
        self.cancel_turn(cx);
    }

    fn reset_visible_state(&mut self) {
        self.reasoning_expanded.clear();
        self.activities_expanded.clear();
        self.expanded_activity_items.clear();
        self.pending_revert = None;
        self.toast = None;
        self.transcript_anchor.set(None);
        self.transcript_anchor_end_space.set(Pixels::ZERO);
        self.transcript_anchor_following.set(false);
        self.transcript_exact_measurement_rows.borrow_mut().clear();
    }

    fn reset_session_runtime(&mut self, session_id: Uuid) {
        if let Some(runtime) = self.runtimes.remove(&session_id) {
            runtime.driver.cancel();
        }
    }

    fn choose_model(&mut self, provider: ProviderKind, model: String, cx: &mut Context<Self>) {
        if let Some(session) = self.selected_session_mut()
            && session.messages.is_empty()
        {
            let session_id = session.id;
            session.provider = provider;
            session.model = Some(model);
            session.reasoning_effort = None;
            session.service_tier = None;
            self.state.last_provider = provider;
            self.model_picker_tab = ModelPickerTab::Provider(provider);
            self.reset_session_runtime(session_id);
            self.save();
            cx.notify();
        }
    }

    fn select_model_picker_tab(&mut self, tab: ModelPickerTab, cx: &mut Context<Self>) {
        if self.model_picker_tab != tab {
            self.model_picker_tab = tab;
            cx.notify();
        }
    }

    fn toggle_favorite_model(
        &mut self,
        provider: ProviderKind,
        model: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self
            .state
            .favorite_models
            .iter()
            .position(|favorite| favorite.provider == provider && favorite.model == model)
        {
            self.state.favorite_models.remove(index);
        } else {
            self.state
                .favorite_models
                .push(FavoriteModel { provider, model });
        }
        self.save();
        cx.notify();
    }

    fn set_runtime_mode(&mut self, mode: RuntimeMode, cx: &mut Context<Self>) {
        if mode == RuntimeMode::Plan {
            return;
        }
        if let Some(session) = self.selected_session_mut()
            && session.runtime_mode != mode
        {
            let session_id = session.id;
            session.runtime_mode = mode;
            self.reset_session_runtime(session_id);
            self.save();
            cx.notify();
        }
    }

    fn set_interaction_mode(&mut self, mode: InteractionMode, cx: &mut Context<Self>) {
        if let Some(session) = self.selected_session_mut()
            && session.interaction_mode != mode
        {
            let session_id = session.id;
            session.interaction_mode = mode;
            self.reset_session_runtime(session_id);
            self.save();
            cx.notify();
        }
    }

    fn set_reasoning_effort(&mut self, effort: String, cx: &mut Context<Self>) {
        if let Some(session) = self.selected_session_mut()
            && session.reasoning_effort.as_deref() != Some(effort.as_str())
        {
            let session_id = session.id;
            session.reasoning_effort = Some(effort);
            self.reset_session_runtime(session_id);
            self.save();
            cx.notify();
        }
    }

    fn set_service_tier(&mut self, tier: String, cx: &mut Context<Self>) {
        if let Some(session) = self.selected_session_mut()
            && session.service_tier.as_deref() != Some(tier.as_str())
        {
            let session_id = session.id;
            session.service_tier = Some(tier);
            self.reset_session_runtime(session_id);
            self.save();
            cx.notify();
        }
    }

    fn cancel_turn(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.state.selected_session else {
            return;
        };
        let mut runtime = self.runtimes.remove(&session_id);
        if let Some(runtime) = runtime.as_ref() {
            runtime.driver.cancel();
        }
        // Do not leave already-received text in the smoothing queue: once the
        // message is marked complete, a later delta would otherwise create a
        // second assistant bubble. Show the received portion immediately.
        let mut keep_runtime = true;
        if let Some(runtime) = runtime.as_mut() {
            Self::collect_runtime_events(runtime);
            while let Some(event) = runtime.pending_events.pop_front() {
                keep_runtime &= self.handle_driver_event(session_id, runtime, event);
                if !keep_runtime {
                    break;
                }
            }
        }
        let has_active_turn = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(AgentSession::active_turn_id)
            .is_some();
        self.finish_streaming_assistant(session_id);
        self.complete_turn_blocks(session_id);
        if let Some(runtime) = runtime.as_mut() {
            runtime.stream_phase = None;
            runtime.pending_permission = None;
        }
        if has_active_turn {
            let needs_fallback = !self.turn_has_assistant_message(session_id);
            if let Some(session) = self
                .state
                .sessions
                .iter_mut()
                .find(|session| session.id == session_id)
            {
                session.status = SessionStatus::Idle;
                if needs_fallback {
                    session.push_message(MessageRole::Assistant, "Stopped.");
                }
                session.finish_active_turn(TurnStatus::Interrupted);
            }
        }
        if has_active_turn {
            self.capture_latest_turn_checkpoint_for(session_id);
        }
        if keep_runtime && let Some(runtime) = runtime {
            self.runtimes.insert(session_id, runtime);
        }
        self.remeasure_transcript_tail();
        self.save();
        cx.notify();
    }

    fn respond_permission(
        &mut self,
        request_id: String,
        option_id: String,
        cx: &mut Context<Self>,
    ) {
        let Some(session_id) = self.state.selected_session else {
            return;
        };
        if let Some(runtime) = self.runtimes.get_mut(&session_id) {
            runtime.driver.respond(request_id, option_id);
            runtime.pending_permission = None;
        }
        if let Some(session) = self.selected_session_mut() {
            session.status = SessionStatus::Working;
        }
        cx.notify();
    }

    fn add_project(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add project".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update(cx, |this, cx| {
                    if let Some(existing) = this.state.projects.iter().find(|p| p.path == path) {
                        this.select_project(existing.id, cx);
                        return;
                    }
                    let project = Project::from_path(path);
                    let project_id = project.id;
                    this.state.projects.push(project);
                    this.create_session_for(project_id, this.state.last_provider, cx);
                });
            }
        })
        .detach();
    }

    // ── Sidebar ────────────────────────────────────────────────────────────

    fn render_sidebar(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::dark();
        let selected_project = self.state.selected_project;
        let selected_session = self.state.selected_session;

        let mut projects = div().flex().flex_col();
        for project in &self.state.projects {
            let project_id = project.id;
            let selected = selected_project == Some(project.id);
            projects = projects.child(
                div()
                    .id(SharedString::from(format!("project-{}", project.id)))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .h(px(28.0))
                    .px(px(8.0))
                    .text_size(px(12.5))
                    .line_height(px(16.0))
                    .rounded(px(7.0))
                    .cursor_default()
                    .when(selected, |element| element.bg(theme.overlay))
                    .hover(|element| element.bg(theme.overlay))
                    .active(|element| element.bg(theme.overlay_strong))
                    .child(icon(
                        "icons/folder.svg",
                        13.0,
                        if selected {
                            theme.text_secondary
                        } else {
                            theme.text_tertiary
                        },
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.5))
                            .text_color(if selected {
                                theme.text
                            } else {
                                theme.text_secondary
                            })
                            .child(SharedString::from(project.name.clone())),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_project(project_id, cx);
                    })),
            );
        }

        let mut sessions = div().flex().flex_col().gap(px(1.0));
        if let Some(project_id) = selected_project {
            let mut project_sessions = self
                .state
                .sessions
                .iter()
                .filter(|session| session.project_id == project_id)
                .collect::<Vec<_>>();
            project_sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at));
            for session in project_sessions {
                let session_id = session.id;
                let selected = selected_session == Some(session.id);
                let active = !matches!(session.status, SessionStatus::Idle);
                let waku = cx.entity().downgrade();
                let composer = self.composer.clone();
                sessions = sessions.child(
                    div()
                        .id(SharedString::from(format!("session-{}", session.id)))
                        .flex()
                        .flex_col()
                        .gap(px(3.0))
                        .px(px(8.0))
                        .py(px(6.0))
                        .rounded(px(7.0))
                        .cursor_default()
                        .when(selected, |element| element.bg(theme.overlay))
                        .hover(|element| element.bg(theme.overlay))
                        .active(|element| element.bg(theme.overlay_strong))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .line_height(px(16.0))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(12.5))
                                        .text_color(if selected {
                                            theme.text
                                        } else {
                                            theme.text_secondary
                                        })
                                        .child(SharedString::from(session.title.clone())),
                                )
                                .when(active, |element| {
                                    element.child(pulse_dot(
                                        format!("session-pulse-{session_id}"),
                                        5.0,
                                        status_color(&theme, session.status),
                                    ))
                                })
                                .child(
                                    div()
                                        .flex_none()
                                        .text_size(px(10.0))
                                        .text_color(theme.text_ghost)
                                        .child(SharedString::from(relative_time(
                                            session.updated_at,
                                        ))),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(5.0))
                                .text_size(px(10.5))
                                .line_height(px(13.0))
                                .child(icon(
                                    provider_icon(session.provider),
                                    10.0,
                                    provider_color(session.provider).opacity(0.8),
                                ))
                                .child(
                                    div()
                                        .text_color(theme.text_tertiary)
                                        .child(session.provider.short_name()),
                                ),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select_session(session_id, cx);
                        }))
                        .context_menu_with_id(
                            SharedString::from(format!("session-context-menu-{session_id}")),
                            move |menu, window, cx| {
                                let waku = waku.clone();
                                preserve_composer_focus_for_context_menu(
                                    &composer, menu, window, cx,
                                )
                                .min_w(px(140.0))
                                .item(
                                    PopupMenuItem::new("Remove").on_click(move |_, _, cx| {
                                        let _ = waku.update(cx, |waku, cx| {
                                            waku.remove_session(session_id, cx);
                                        });
                                    }),
                                )
                            },
                        ),
                );
            }
        }

        div()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .child(div().h(px(48.0)).flex_none())
            .child(
                div().px(px(10.0)).child(
                    div()
                        .id("new-session")
                        .h(px(30.0))
                        .px(px(8.0))
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .text_size(px(12.5))
                        .line_height(px(16.0))
                        .rounded(px(7.0))
                        .cursor_default()
                        .hover(|element| element.bg(theme.overlay))
                        .active(|element| element.bg(theme.overlay_strong))
                        .child(icon("icons/plus.svg", 13.0, theme.text_secondary))
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(12.5))
                                .text_color(theme.text)
                                .child("New session"),
                        )
                        .child(key_hint(&theme, "⌘N"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(project_id) = this.state.selected_project {
                                this.create_session_for(project_id, this.state.last_provider, cx);
                            }
                        })),
                ),
            )
            .child(
                div()
                    .id("sidebar-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .px(px(10.0))
                    .pt(px(14.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(section_label(&theme, "Projects"))
                            .child(
                                div()
                                    .id("add-project")
                                    .w(px(20.0))
                                    .h(px(20.0))
                                    .rounded(px(6.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_default()
                                    .hover(|element| element.bg(theme.overlay))
                                    .active(|element| element.bg(theme.overlay_strong))
                                    .child(icon("icons/plus.svg", 11.0, theme.text_ghost))
                                    .on_click(cx.listener(|this, _, _, cx| this.add_project(cx))),
                            ),
                    )
                    .child(projects)
                    .child(div().h(px(16.0)))
                    .child(section_label(&theme, "Sessions"))
                    .child(sessions),
            )
            .child(
                div()
                    .h(px(40.0))
                    .flex_none()
                    .px(px(18.0))
                    .flex()
                    .items_center()
                    .line_height(px(13.0))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(10.5))
                            .text_color(theme.text_ghost)
                            .child("Local only"),
                    )
                    .child(key_hint(&theme, "⌘⇧S")),
            )
    }

    // ── Header ─────────────────────────────────────────────────────────────

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
        let session = self.selected_session();
        let provider = session.map(|session| session.provider).unwrap_or_default();
        let status = session.map(|session| session.status).unwrap_or_default();
        div()
            .id("window-header")
            .h(px(46.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.0))
            .pl(if self.sidebar_visible {
                px(10.0)
            } else {
                px(TRAFFIC_LIGHT_CLEARANCE)
            })
            .pr(px(14.0))
            .border_b_1()
            .border_color(theme.border)
            .on_click(|event, window, _| {
                if event.click_count() == 2 {
                    window.titlebar_double_click();
                }
            })
            .on_mouse_down_out(cx.listener(|this, _, _, _| {
                this.header_drag_armed = false;
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.header_drag_armed = true;
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.header_drag_armed = false;
                }),
            )
            .on_mouse_move(cx.listener(|this, _, window, _| {
                if this.header_drag_armed {
                    this.header_drag_armed = false;
                    crate::platform::start_window_move(window);
                }
            }))
            .child(
                div()
                    .id("toggle-sidebar")
                    .w(px(26.0))
                    .h(px(26.0))
                    .flex_none()
                    .rounded(px(6.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_default()
                    .hover(|element| element.bg(theme.overlay))
                    .active(|element| element.bg(theme.overlay_strong))
                    .child(icon("icons/panel-left.svg", 14.0, theme.text_tertiary))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.sidebar_visible = !this.sidebar_visible;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(SharedString::from(
                        session
                            .map(|session| session.title.clone())
                            .unwrap_or_else(|| "New task".into()),
                    )),
            )
            .child(div().flex_1())
            .when(status != SessionStatus::Idle, |element| {
                element.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .text_size(px(11.0))
                        .line_height(px(14.0))
                        .child(match status {
                            SessionStatus::Connecting | SessionStatus::Working => {
                                pulse_dot("header-status-pulse", 5.0, status_color(&theme, status))
                            }
                            _ => div()
                                .w(px(5.0))
                                .h(px(5.0))
                                .rounded_full()
                                .bg(status_color(&theme, status))
                                .into_any_element(),
                        })
                        .child(
                            div()
                                .text_color(status_color(&theme, status))
                                .child(status_label(status)),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(11.5))
                    .line_height(px(14.0))
                    .child(icon(
                        provider_icon(provider),
                        11.0,
                        provider_color(provider).opacity(0.9),
                    ))
                    .child(
                        div()
                            .text_color(theme.text_secondary)
                            .child(provider.short_name()),
                    ),
            )
    }

    // ── Empty states ───────────────────────────────────────────────────────

    fn render_empty_state(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::dark();
        if self.selected_project().is_none() {
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .px_8()
                .pb(px(46.0))
                .child(icon("icons/sparkle.svg", 24.0, theme.accent))
                .child(
                    div()
                        .mt(px(16.0))
                        .text_size(px(20.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child("Open a project to begin"),
                )
                .child(
                    div()
                        .mt(px(8.0))
                        .max_w(px(380.0))
                        .text_center()
                        .text_size(px(12.5))
                        .line_height(px(19.0))
                        .text_color(theme.text_tertiary)
                        .child(
                            "Waku runs coding agents in folders you choose. Your code, sessions, and history stay on this Mac.",
                        ),
                )
                .child(
                    div()
                        .id("onboarding-add-project")
                        .mt(px(20.0))
                        .h(px(32.0))
                        .px(px(14.0))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .cursor_default()
                        .bg(theme.inverse)
                        .text_color(theme.on_inverse)
                        .text_size(px(12.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .hover(|element| element.opacity(0.9))
                        .active(|element| element.opacity(0.8))
                        .child("Open project folder…")
                        .on_click(cx.listener(|this, _, _, cx| this.add_project(cx))),
                );
        }
        let project_name = self
            .selected_project()
            .map(|project| project.name.as_str())
            .unwrap_or("your project");
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px_8()
            .pb(px(52.0))
            .child(icon("icons/sparkle.svg", 20.0, theme.accent))
            .child(
                div()
                    .mt(px(14.0))
                    .text_size(px(20.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(SharedString::from(format!(
                        "What should we build in {project_name}?"
                    ))),
            )
    }

    // ── Transcript ─────────────────────────────────────────────────────────

    fn render_transcript(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        self.sync_transcript_rows();
        if self.sync_transcript_layout_width(window) {
            while self.transcript_resize_rx.try_recv().is_ok() {}
        } else {
            self.drain_transcript_resize_events();
        }
        let transcript_rows = self.active_transcript_rows().clone();
        let anchor_end_space = self.update_transcript_anchor_end_space(window);
        if self.transcript_anchor_following.get()
            && anchor_end_space <= Pixels::ZERO
            && self
                .selected_transcript_anchor_row()
                .is_some_and(|anchor_row| anchor_row + 1 < transcript_rows.item_count())
        {
            transcript_rows.scroll_to(ListOffset {
                item_ix: transcript_rows.item_count(),
                offset_in_item: Pixels::ZERO,
            });
            self.transcript_is_scrolled.set(false);
        }
        let entity = cx.entity().downgrade();
        let transcript_viewport = TextViewScrollViewport::from_list(&transcript_rows);
        let scrollbar_handle = StableListScrollbarHandle::new(
            &transcript_rows,
            &self.transcript_estimated_height,
            &self.transcript_anchor_end_space,
            &self.transcript_anchor_following,
            &self.transcript_drag_estimated_height,
            &self.transcript_is_scrolled,
        );
        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .relative()
            .child(
                list(transcript_rows, move |index, _window, cx| {
                    entity
                        .upgrade()
                        .map(|entity| {
                            entity.update(cx, |this, cx| {
                                this.transcript_row(index, transcript_viewport, cx)
                            })
                        })
                        .unwrap_or_else(|| div().into_any_element())
                })
                .size_full()
                .pb(anchor_end_space),
            )
            .vertical_scrollbar(&scrollbar_handle)
            .into_any_element()
    }

    /// The provider's latest ordered block is still reasoning.
    fn reasoning_live(&self) -> bool {
        self.selected_runtime()
            .is_some_and(|runtime| runtime.stream_phase == Some(StreamPhase::Reasoning))
            && self
                .selected_session()
                .is_some_and(|session| session.status == SessionStatus::Working)
    }

    fn toggle_reasoning(&mut self, block_index: usize, current: bool, cx: &mut Context<Self>) {
        self.reasoning_expanded.insert(block_index, !current);
        self.remeasure_transcript_block(block_index);
        cx.notify();
    }

    fn toggle_activities(&mut self, block_index: usize, current: bool, cx: &mut Context<Self>) {
        self.activities_expanded.insert(block_index, !current);
        self.remeasure_transcript_block(block_index);
        cx.notify();
    }

    fn toggle_activity_item(&mut self, id: Uuid, cx: &mut Context<Self>) {
        if !self.expanded_activity_items.remove(&id) {
            self.expanded_activity_items.insert(id);
        }
        if let Some(block_index) = self.selected_transcript_blocks().iter().position(|block| {
            matches!(
                &block.content,
                TranscriptBlockContent::Activities(activities)
                    if activities.iter().any(|activity| activity.id == id)
            )
        }) {
            self.remeasure_transcript_block(block_index);
        }
        cx.notify();
    }

    /// A single transcript row, self-centered to the content column so the
    /// list can measure it at its true wrap width. Current-turn reasoning and
    /// activity blocks are anchored at the exact boundary between assistant
    /// text segments where their provider events arrived.
    fn checkpoint_action_for_message(&self, message_index: usize) -> Option<CheckpointAction> {
        let session = self.selected_session()?;
        let message = session.messages.get(message_index)?;
        let turn_id = message.turn_id?;
        if session.messages[message_index + 1..]
            .iter()
            .any(|later| later.turn_id == Some(turn_id))
        {
            return None;
        }
        let turn = session.turns.iter().find(|turn| turn.id == turn_id)?;
        let checkpoint = turn
            .checkpoint
            .as_ref()
            .filter(|checkpoint| checkpoint.status == CheckpointStatus::Ready)?;
        Some(CheckpointAction {
            session_id: session.id,
            turn_count: turn.turn_count,
            file_count: checkpoint.files.len(),
            can_revert: session.status == SessionStatus::Idle
                && session.provider.supports_conversation_rollback()
                && session.provider_cursor.is_some(),
            confirmed: self.pending_revert == Some((session.id, turn.turn_count)),
        })
    }

    fn transcript_row(
        &mut self,
        index: usize,
        transcript_viewport: TextViewScrollViewport,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.transcript_provisional_rows.borrow_mut().remove(&index) {
            if self
                .selected_transcript_anchor_row()
                .is_some_and(|anchor_row| index >= anchor_row)
            {
                self.transcript_exact_measurement_rows
                    .borrow_mut()
                    .insert(index);
                cx.notify();
            }
            let estimated_height = self
                .transcript_row_estimates
                .borrow()
                .get(index)
                .copied()
                .unwrap_or(px(44.0));
            return div()
                .w_full()
                .h(estimated_height)
                .flex_none()
                .into_any_element();
        }
        if self
            .transcript_exact_measurement_rows
            .borrow_mut()
            .remove(&index)
        {
            // This render replaces the provisional element. Schedule one more
            // pass so the anchor reservation reads the exact post-layout row
            // bounds instead of leaving the estimate in place indefinitely.
            cx.notify();
        }

        let theme = Theme::dark();
        let composer = self.composer.clone();
        let waku = cx.entity().downgrade();
        let row_count = self.transcript_row_count();
        let kind = self
            .transcript_row_kinds
            .borrow()
            .get(index)
            .copied()
            .unwrap_or(TranscriptRowKind::Message(index));
        let starts_followup_turn = match kind {
            TranscriptRowKind::Message(message_index) => {
                self.selected_session().is_some_and(|session| {
                    message_starts_followup_turn(&session.messages, message_index)
                })
            }
            TranscriptRowKind::TurnBlock(_) => false,
        };
        let inner = match kind {
            TranscriptRowKind::Message(message_index) => self
                .selected_session()
                .and_then(|session| session.messages.get(message_index))
                .cloned()
                .map(|message| {
                    let checkpoint_action = self.checkpoint_action_for_message(message_index);
                    let text_state = self
                        .message_text_states
                        .entry(message.id)
                        .or_insert_with(|| cx.new(TextViewState::new))
                        .clone();
                    render_message(
                        &theme,
                        &message,
                        checkpoint_action,
                        self.state.selected_session.unwrap_or_default(),
                        self.transcript_resize_tx.clone(),
                        self.transcript_layout_width.get(),
                        self.active_transcript_rows().clone(),
                        transcript_viewport,
                        text_state,
                        waku,
                        composer,
                        cx,
                    )
                })
                .unwrap_or_else(|| div().into_any_element()),
            TranscriptRowKind::TurnBlock(block_index) => self
                .selected_transcript_blocks()
                .get(block_index)
                .map(|block| match &block.content {
                    TranscriptBlockContent::Reasoning(reasoning) => {
                        self.render_reasoning_row(reasoning, block_index, &theme, cx)
                    }
                    TranscriptBlockContent::Activities(activities) => {
                        self.render_activities_row(activities, block_index, &theme, cx)
                    }
                })
                .unwrap_or_else(|| div().into_any_element()),
        };
        div()
            .w_full()
            .flex()
            .justify_center()
            .px(px(20.0))
            .py(px(8.0))
            .when(index == 0, |element| element.pt(px(22.0)))
            .when(starts_followup_turn, |element| {
                element.pt(px(FOLLOWUP_TURN_TOP_GAP))
            })
            .when(index + 1 == row_count, |element| element.pb(px(22.0)))
            .child(
                div()
                    .w_full()
                    .max_w(px(CONTENT_MAX_WIDTH))
                    .min_w_0()
                    .child(inner),
            )
            .into_any_element()
    }

    /// The turn's reasoning as a disclosure: open while the provider is
    /// thinking, collapsing to "Thought for Ns" once the answer starts, and
    /// clickable either way.
    fn render_reasoning_row(
        &self,
        reasoning: &ReasoningBlock,
        block_index: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let live =
            self.reasoning_live()
                && self.selected_transcript_blocks().iter().rposition(|block| {
                    matches!(block.content, TranscriptBlockContent::Reasoning(_))
                }) == Some(block_index);
        let expanded = self
            .reasoning_expanded
            .get(&block_index)
            .copied()
            .unwrap_or(live);
        let label = if live {
            "Thinking".to_owned()
        } else {
            format!(
                "Thought for {}s",
                reasoning
                    .finished_at_ms
                    .saturating_sub(reasoning.started_at_ms)
                    .div_ceil(1000)
                    .max(1)
            )
        };
        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .id(SharedString::from(format!("thinking-toggle-{block_index}")))
                    .h(px(22.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(11.0))
                    .line_height(px(14.0))
                    .cursor_default()
                    .child(icon(
                        if expanded {
                            "icons/chevron-down.svg"
                        } else {
                            "icons/chevron-right.svg"
                        },
                        9.0,
                        theme.text_ghost,
                    ))
                    .child(if live {
                        icon("icons/sparkle.svg", 11.0, theme.text_tertiary)
                            .with_animation(
                                SharedString::from(format!("thinking-pulse-{block_index}")),
                                Animation::new(Duration::from_millis(1800))
                                    .repeat()
                                    .with_easing(pulsating_between(0.4, 1.0)),
                                |element, delta| element.opacity(delta),
                            )
                            .into_any_element()
                    } else {
                        icon("icons/sparkle.svg", 11.0, theme.text_ghost).into_any_element()
                    })
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text_tertiary)
                            .child(SharedString::from(label)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_reasoning(block_index, expanded, cx);
                    })),
            )
            .when(expanded, |element| {
                element.child(
                    div()
                        .pl(px(15.0))
                        .text_size(px(12.0))
                        .line_height(px(18.0))
                        .text_color(theme.text_tertiary)
                        .whitespace_normal()
                        .child(SharedString::from(reasoning.content.clone())),
                )
            })
            .into_any_element()
    }

    /// The turn's tool activity as a disclosure: the summary line toggles the
    /// row list, and each row with detail expands to its full content.
    fn render_activities_row(
        &self,
        activities: &[ActivityItem],
        block_index: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let running = activities.iter().any(|activity| !activity.complete);
        let expanded = self
            .activities_expanded
            .get(&block_index)
            .copied()
            .unwrap_or(running);
        let cluster = div().flex().flex_col().gap(px(2.0)).child(
            div()
                .id(SharedString::from(format!("activity-toggle-{block_index}")))
                .h(px(22.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(px(11.0))
                .line_height(px(14.0))
                .cursor_default()
                .child(icon(
                    if expanded {
                        "icons/chevron-down.svg"
                    } else {
                        "icons/chevron-right.svg"
                    },
                    9.0,
                    theme.text_ghost,
                ))
                .when(running, |element| {
                    element.child(pulse_dot(
                        format!("activity-running-{block_index}"),
                        5.0,
                        theme.accent,
                    ))
                })
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text_tertiary)
                        .child(SharedString::from(activity_summary(activities))),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_activities(block_index, expanded, cx);
                })),
        );
        if !expanded {
            return cluster.into_any_element();
        }
        let mut items = div().flex().flex_col().pl(px(15.0));
        for activity in activities {
            let id = activity.id;
            let detail = activity
                .detail
                .clone()
                .filter(|detail| !detail.trim().is_empty());
            let has_detail = detail.is_some();
            let item_expanded = has_detail && self.expanded_activity_items.contains(&id);
            let mut item = div().flex().flex_col().child(
                div()
                    .id(SharedString::from(format!("activity-item-{id}")))
                    .min_h(px(24.0))
                    .px(px(4.0))
                    .py(px(2.0))
                    .rounded(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(11.5))
                    .line_height(px(14.0))
                    .when(has_detail, |element| {
                        element
                            .cursor_default()
                            .hover(|element| element.bg(theme.overlay))
                            .active(|element| element.bg(theme.overlay_strong))
                    })
                    .child(if has_detail {
                        icon(
                            if item_expanded {
                                "icons/chevron-down.svg"
                            } else {
                                "icons/chevron-right.svg"
                            },
                            9.0,
                            theme.text_ghost,
                        )
                        .into_any_element()
                    } else {
                        div().w(px(9.0)).flex_none().into_any_element()
                    })
                    .child(icon(
                        activity_icon(activity.kind),
                        11.0,
                        theme.text_tertiary,
                    ))
                    .child(
                        div()
                            .flex_none()
                            .max_w(px(300.0))
                            .truncate()
                            .text_color(theme.text_secondary)
                            .child(SharedString::from(activity.title.clone())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.0))
                            .text_color(theme.text_ghost)
                            .when(item_expanded, |element| element.invisible())
                            .child(SharedString::from(detail.clone().unwrap_or_default())),
                    )
                    .child(if activity.complete {
                        icon("icons/check.svg", 10.0, theme.text_ghost).into_any_element()
                    } else {
                        pulse_dot(format!("activity-pulse-{id}"), 5.0, theme.accent)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if has_detail {
                            this.toggle_activity_item(id, cx);
                        }
                    })),
            );
            if let Some(detail) = detail.filter(|_| item_expanded) {
                item = item.child(
                    div()
                        .ml(px(21.0))
                        .mt(px(2.0))
                        .mb(px(4.0))
                        .p(px(8.0))
                        .rounded(px(7.0))
                        .bg(theme.inset)
                        .border_1()
                        .border_color(theme.border)
                        .font_family("SF Mono")
                        .text_size(px(10.5))
                        .line_height(px(16.0))
                        .text_color(theme.text_secondary)
                        .whitespace_normal()
                        .child(SharedString::from(detail)),
                );
            }
            items = items.child(item);
        }
        cluster.child(items).into_any_element()
    }

    // ── Permission ─────────────────────────────────────────────────────────

    fn render_permission(&self, cx: &mut Context<Self>) -> Option<Div> {
        let permission = self.selected_runtime()?.pending_permission.as_ref()?;
        let theme = Theme::dark();
        let request_id = permission.request_id.clone();
        let mut buttons = div().flex().items_center().gap(px(8.0)).mt(px(10.0));
        for option in &permission.options {
            let request_id = request_id.clone();
            let option_id = option.id.clone();
            let allow = option.allow;
            buttons = buttons.child(
                div()
                    .id(SharedString::from(format!(
                        "permission-{}-{}",
                        permission.request_id, option.id
                    )))
                    .h(px(28.0))
                    .px(px(13.0))
                    .rounded(px(7.0))
                    .flex()
                    .items_center()
                    .cursor_default()
                    .text_size(px(11.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(allow, |element| {
                        element
                            .bg(theme.inverse)
                            .text_color(theme.on_inverse)
                            .hover(|element| element.opacity(0.9))
                    })
                    .when(!allow, |element| {
                        element
                            .border_1()
                            .border_color(theme.border_strong)
                            .text_color(theme.text_secondary)
                            .hover(|element| element.bg(theme.overlay).text_color(theme.text))
                    })
                    .active(|element| element.opacity(0.8))
                    .child(SharedString::from(option.label.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.respond_permission(request_id.clone(), option_id.clone(), cx);
                    })),
            );
        }
        Some(
            div().px(px(20.0)).pb(px(8.0)).child(
                div()
                    .w_full()
                    .max_w(px(CONTENT_MAX_WIDTH))
                    .mx_auto()
                    .p(px(12.0))
                    .rounded(px(12.0))
                    .border_1()
                    .border_color(theme.border_strong)
                    .bg(theme.raised)
                    .shadow_md()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(icon("icons/alert.svg", 13.0, theme.warning))
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(SharedString::from(permission.title.clone())),
                            ),
                    )
                    .child(
                        div()
                            .id("permission-detail")
                            .mt(px(8.0))
                            .max_h(px(92.0))
                            .overflow_y_scroll()
                            .p(px(8.0))
                            .rounded(px(7.0))
                            .bg(theme.inset)
                            .font_family("SF Mono")
                            .text_size(px(10.5))
                            .line_height(px(16.0))
                            .text_color(theme.text_secondary)
                            .whitespace_normal()
                            .child(SharedString::from(permission.detail.clone())),
                    )
                    .child(buttons),
            ),
        )
    }

    // ── Composer ───────────────────────────────────────────────────────────

    fn render_provider_model_control(
        &self,
        fresh_session: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::dark();
        let session = self.selected_session();
        let provider = session.map(|session| session.provider).unwrap_or_default();
        let selected_model = session.and_then(|session| self.model_for_session(session));
        let selected_model_name = self.model_display_name(provider, selected_model);

        if !fresh_session {
            return div()
                .h(px(24.0))
                .px(px(7.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(icon(
                    provider_icon(provider),
                    10.5,
                    provider_color(provider).opacity(0.9),
                ))
                .child(
                    div()
                        .max_w(px(210.0))
                        .truncate()
                        .text_color(theme.text_secondary)
                        .child(SharedString::from(selected_model_name)),
                )
                .into_any_element();
        }

        let search_query = self.model_search.read(cx).value().to_string();
        let normalized_query = search_query.trim().to_ascii_lowercase();
        let searching = !normalized_query.is_empty();
        let selected_tab = self.model_picker_tab;
        let selected_model = selected_model.map(str::to_owned);
        let probes = self.probes.clone();
        let favorites = self.state.favorite_models.clone();
        let weak = cx.entity().downgrade();
        let search = self.model_search.clone();
        let search_focus = search.read(cx).focus_handle(cx);

        let trigger = MenuChip::new("composer-provider-model")
            .icon(
                provider_icon(provider),
                provider_color(provider).opacity(0.9),
            )
            .label(selected_model_name);

        Popover::new("provider-model-picker")
            .anchor(Corner::BottomLeft)
            .appearance(false)
            .track_focus(&search_focus)
            .on_open_change({
                let weak = weak.clone();
                let search = search.clone();
                move |open, window, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        if *open {
                            this.model_picker_tab = ModelPickerTab::Provider(
                                this.selected_session()
                                    .map(|session| session.provider)
                                    .unwrap_or_default(),
                            );
                            search.update(cx, |search, cx| {
                                search.set_value("", window, cx);
                            });
                        } else {
                            window.focus(&this.composer.read(cx).focus());
                        }
                        cx.notify();
                    });
                }
            })
            .trigger(trigger)
            .content(move |_state, _window, popover_cx| {
                let popover = popover_cx.entity();
                let mut available_models = probes
                    .iter()
                    .filter(|probe| probe.installed)
                    .flat_map(|probe| {
                        probe
                            .models
                            .iter()
                            .cloned()
                            .map(move |model| (probe.provider, model))
                    })
                    .filter(|(kind, model)| {
                        if searching {
                            let searchable = format!(
                                "{} {} {} {}",
                                model.name,
                                model.id,
                                kind.short_name(),
                                model.sub_provider.as_deref().unwrap_or("")
                            )
                            .to_ascii_lowercase();
                            return normalized_query
                                .split_whitespace()
                                .all(|token| searchable.contains(token));
                        }
                        match selected_tab {
                            ModelPickerTab::Favorites => favorites.iter().any(|favorite| {
                                favorite.provider == *kind && favorite.model == model.id
                            }),
                            ModelPickerTab::Provider(provider) => provider == *kind,
                        }
                    })
                    .collect::<Vec<_>>();
                if !searching && selected_tab == ModelPickerTab::Favorites {
                    available_models.sort_by_key(|(kind, model)| {
                        favorites
                            .iter()
                            .position(|favorite| {
                                favorite.provider == *kind && favorite.model == model.id
                            })
                            .unwrap_or(usize::MAX)
                    });
                }

                let mut sidebar = div()
                    .w(px(50.0))
                    .h_full()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(4.0))
                    .p(px(5.0))
                    .rounded_tl(px(12.0))
                    .rounded_bl(px(12.0))
                    .bg(theme.canvas)
                    .border_r_1()
                    .border_color(theme.border);

                let favorites_selected = selected_tab == ModelPickerTab::Favorites && !searching;
                let favorite_weak = weak.clone();
                sidebar = sidebar
                    .child(
                        div()
                            .id("model-tab-favorites")
                            .w(px(38.0))
                            .h(px(38.0))
                            .rounded(px(7.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_default()
                            .when(favorites_selected, |element| {
                                element.bg(theme.overlay_strong)
                            })
                            .hover(|element| element.bg(theme.overlay))
                            .child(icon(
                                "icons/star.svg",
                                17.0,
                                if favorites_selected {
                                    theme.text
                                } else {
                                    theme.text_tertiary
                                },
                            ))
                            .on_click(move |_, _, cx| {
                                let _ = favorite_weak.update(cx, |this, cx| {
                                    this.select_model_picker_tab(ModelPickerTab::Favorites, cx);
                                });
                            }),
                    )
                    .child(div().w(px(34.0)).h(px(1.0)).my(px(3.0)).bg(theme.border));

                for kind in ProviderKind::ALL {
                    let installed = probes
                        .iter()
                        .find(|probe| probe.provider == kind)
                        .map(|probe| probe.installed)
                        .unwrap_or(false);
                    let selected = selected_tab == ModelPickerTab::Provider(kind) && !searching;
                    let tab_weak = weak.clone();
                    sidebar = sidebar.child(
                        div()
                            .id(SharedString::from(format!("model-tab-{}", kind.id())))
                            .w(px(38.0))
                            .h(px(38.0))
                            .rounded(px(7.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_default()
                            .when(selected, |element| element.bg(theme.overlay_strong))
                            .when(!installed, |element| element.opacity(0.35))
                            .when(installed, |element| {
                                element.hover(|element| element.bg(theme.overlay)).on_click(
                                    move |_, _, cx| {
                                        let _ = tab_weak.update(cx, |this, cx| {
                                            this.select_model_picker_tab(
                                                ModelPickerTab::Provider(kind),
                                                cx,
                                            );
                                        });
                                    },
                                )
                            })
                            .child(icon(
                                provider_icon(kind),
                                18.0,
                                provider_color(kind).opacity(if selected { 1.0 } else { 0.82 }),
                            )),
                    );
                }

                let search_input = div()
                    .h(px(48.0))
                    .mx(px(14.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border_strong)
                    .child(
                        Input::new(&search)
                            .appearance(false)
                            .bordered(false)
                            .focus_bordered(false)
                            .prefix(icon("icons/search.svg", 15.0, theme.text_tertiary)),
                    );

                let mut rows = div()
                    .id("model-picker-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(px(9.0));
                if available_models.is_empty() {
                    let label = if searching {
                        "No models found"
                    } else if selected_tab == ModelPickerTab::Favorites {
                        "Star a model to keep it here"
                    } else {
                        "No models reported by this provider"
                    };
                    rows = rows.child(
                        div()
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(11.5))
                            .text_color(theme.text_ghost)
                            .child(label),
                    );
                }

                for (kind, model) in available_models {
                    let is_selected =
                        kind == provider && selected_model.as_deref() == Some(model.id.as_str());
                    let is_favorite = favorites
                        .iter()
                        .any(|favorite| favorite.provider == kind && favorite.model == model.id);
                    let model_id = model.id.clone();
                    let select_weak = weak.clone();
                    let select_popover = popover.clone();
                    let favorite_model_id = model.id.clone();
                    let favorite_weak = weak.clone();
                    let subtitle = model.sub_provider.as_deref().map_or_else(
                        || kind.short_name().to_owned(),
                        |sub_provider| format!("{sub_provider} · {}", kind.short_name()),
                    );
                    rows = rows.child(
                        div()
                            .id(SharedString::from(format!(
                                "model-row-{}-{}",
                                kind.id(),
                                model.id
                            )))
                            .h(px(58.0))
                            .px(px(12.0))
                            .rounded(px(9.0))
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .cursor_default()
                            .when(is_selected, |element| element.bg(theme.overlay_strong))
                            .hover(|element| element.bg(theme.overlay))
                            .active(|element| element.opacity(0.85))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(px(13.0))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme.text)
                                            .child(SharedString::from(model.name)),
                                    )
                                    .child(
                                        div()
                                            .mt(px(4.0))
                                            .flex()
                                            .items_center()
                                            .gap(px(6.0))
                                            .child(icon(
                                                provider_icon(kind),
                                                10.5,
                                                provider_color(kind).opacity(0.85),
                                            ))
                                            .child(
                                                div()
                                                    .truncate()
                                                    .text_size(px(11.0))
                                                    .text_color(theme.text_tertiary)
                                                    .child(SharedString::from(subtitle)),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!(
                                        "favorite-model-{}-{}",
                                        kind.id(),
                                        model.id
                                    )))
                                    .w(px(28.0))
                                    .h(px(28.0))
                                    .rounded(px(6.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .hover(|element| element.bg(theme.overlay_strong))
                                    .child(icon(
                                        "icons/star.svg",
                                        14.0,
                                        if is_favorite {
                                            theme.text_secondary
                                        } else {
                                            theme.text_ghost
                                        },
                                    ))
                                    .on_click(move |_, _, cx| {
                                        cx.stop_propagation();
                                        let _ = favorite_weak.update(cx, |this, cx| {
                                            this.toggle_favorite_model(
                                                kind,
                                                favorite_model_id.clone(),
                                                cx,
                                            );
                                        });
                                    }),
                            )
                            .on_click(move |_, window, cx| {
                                let _ = select_weak.update(cx, |this, cx| {
                                    this.choose_model(kind, model_id.clone(), cx);
                                });
                                select_popover.update(cx, |popover, cx| {
                                    popover.dismiss(window, cx);
                                });
                            }),
                    );
                }

                div()
                    .w(px(460.0))
                    .h(px(390.0))
                    .rounded(px(13.0))
                    .overflow_hidden()
                    .border_1()
                    .border_color(theme.border_strong)
                    .bg(theme.raised)
                    .shadow_lg()
                    .flex()
                    .child(sidebar)
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .rounded_tr(px(12.0))
                            .rounded_br(px(12.0))
                            .bg(theme.surface)
                            .child(search_input)
                            .child(rows),
                    )
            })
            .into_any_element()
    }

    fn render_model_traits_control(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session = self.selected_session()?;
        let model = self.model_metadata_for_session(session)?;
        if model.reasoning_efforts.is_empty() && model.service_tiers.is_empty() {
            return None;
        }

        let selected_effort = session
            .reasoning_effort
            .as_deref()
            .filter(|selected| {
                model
                    .reasoning_efforts
                    .iter()
                    .any(|option| option.id == *selected)
            })
            .or(model.default_reasoning_effort.as_deref())
            .or_else(|| {
                model
                    .reasoning_efforts
                    .first()
                    .map(|option| option.id.as_str())
            })
            .map(str::to_owned);
        let effort_label = selected_effort.as_deref().and_then(|selected| {
            model
                .reasoning_efforts
                .iter()
                .find(|option| option.id == selected)
                .map(|option| option.label.clone())
        });

        let selected_tier = session
            .service_tier
            .as_deref()
            .filter(|selected| {
                *selected == "default"
                    || model
                        .service_tiers
                        .iter()
                        .any(|option| option.id == *selected)
            })
            .or(model.default_service_tier.as_deref())
            .unwrap_or("default")
            .to_owned();
        let tier_label = if selected_tier == "default" {
            "Standard".to_owned()
        } else {
            model
                .service_tiers
                .iter()
                .find(|option| option.id == selected_tier)
                .map(|option| option.label.clone())
                .unwrap_or_else(|| selected_tier.clone())
        };
        let fast = selected_tier == "fast" || tier_label.eq_ignore_ascii_case("fast");
        let trigger_label = effort_label.unwrap_or_else(|| tier_label.clone());
        let reasoning_efforts = model.reasoning_efforts.clone();
        let default_effort = model.default_reasoning_effort.clone();
        let service_tiers = model.service_tiers.clone();
        let default_tier = model
            .default_service_tier
            .clone()
            .unwrap_or_else(|| "default".to_owned());
        let weak = cx.entity().downgrade();
        let composer = self.composer.clone();
        let trigger = MenuChip::new("model-traits")
            .when(fast, |trigger| {
                trigger.icon("icons/zap.svg", Theme::dark().text_secondary)
            })
            .label(trigger_label);

        Some(
            trigger
                .dropdown_menu(move |mut menu, _window, cx| {
                    menu = menu
                        .action_context(composer.read(cx).focus())
                        .min_w(px(208.0))
                        .max_w(px(208.0));
                    if !reasoning_efforts.is_empty() {
                        menu = menu.item(traits_menu_label(Theme::dark(), "Reasoning"));
                        for option in reasoning_efforts.clone() {
                            let checked = selected_effort.as_deref() == Some(option.id.as_str());
                            let is_default = default_effort.as_deref() == Some(option.id.as_str());
                            let effort = option.id;
                            let item_weak = weak.clone();
                            menu = menu.item(
                                traits_menu_choice(
                                    Theme::dark(),
                                    option.label,
                                    is_default,
                                    checked,
                                )
                                .on_click(move |_, _, cx| {
                                    let _ = item_weak.update(cx, |this, cx| {
                                        this.set_reasoning_effort(effort.clone(), cx);
                                    });
                                }),
                            );
                        }
                    }
                    if !service_tiers.is_empty() {
                        if !reasoning_efforts.is_empty() {
                            menu = menu.separator();
                        }
                        menu = menu.item(traits_menu_label(Theme::dark(), "Service Tier"));
                        let standard_weak = weak.clone();
                        menu = menu.item(
                            traits_menu_choice(
                                Theme::dark(),
                                "Standard".to_owned(),
                                default_tier == "default",
                                selected_tier == "default",
                            )
                            .on_click(move |_, _, cx| {
                                let _ = standard_weak.update(cx, |this, cx| {
                                    this.set_service_tier("default".to_owned(), cx);
                                });
                            }),
                        );
                        for option in service_tiers.clone() {
                            let checked = selected_tier == option.id;
                            let is_default = default_tier == option.id;
                            let tier = option.id;
                            let item_weak = weak.clone();
                            menu = menu.item(
                                traits_menu_choice(
                                    Theme::dark(),
                                    option.label,
                                    is_default,
                                    checked,
                                )
                                .on_click(move |_, _, cx| {
                                    let _ = item_weak.update(cx, |this, cx| {
                                        this.set_service_tier(tier.clone(), cx);
                                    });
                                }),
                            );
                        }
                    }
                    menu
                })
                .anchor(Corner::BottomLeft)
                .into_any_element(),
        )
    }

    fn render_access_control(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::dark();
        let selected_mode = self
            .selected_session()
            .map(|session| session.runtime_mode)
            .filter(|mode| *mode != RuntimeMode::Plan)
            .unwrap_or_default();
        let weak = cx.entity().downgrade();
        let composer = self.composer.clone();
        MenuChip::new("runtime-mode")
            .icon(selected_mode.icon(), theme.text_tertiary)
            .label(selected_mode.label())
            .dropdown_menu(move |mut menu, _window, cx| {
                menu = menu
                    .action_context(composer.read(cx).focus())
                    .min_w(px(320.0))
                    .max_w(px(320.0));
                for option in RuntimeMode::ACCESS_OPTIONS {
                    let item_weak = weak.clone();
                    let item_theme = theme;
                    let selected = option == selected_mode;
                    menu = menu.item(
                        PopupMenuItem::element(move |_, _| {
                            div()
                                .w_full()
                                .px(px(4.0))
                                .py(px(3.0))
                                .rounded(px(6.0))
                                .flex()
                                .items_center()
                                .gap(px(10.0))
                                .child(icon(option.icon(), 14.0, item_theme.text_tertiary))
                                .child(
                                    div()
                                        .w(px(272.0))
                                        .flex_none()
                                        .child(
                                            div()
                                                .w_full()
                                                .truncate()
                                                .text_size(px(12.0))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(item_theme.text)
                                                .child(option.label()),
                                        )
                                        .child(
                                            div()
                                                .w_full()
                                                .mt(px(2.0))
                                                .text_size(px(10.5))
                                                .line_height(px(14.0))
                                                .whitespace_normal()
                                                .text_color(item_theme.text_tertiary)
                                                .child(option.description()),
                                        ),
                                )
                        })
                        .selected(selected)
                        .on_click(move |_, _, cx| {
                            let _ = item_weak.update(cx, |this, cx| {
                                this.set_runtime_mode(option, cx);
                            });
                        }),
                    );
                }
                menu
            })
            .anchor(Corner::BottomLeft)
            .into_any_element()
    }

    fn render_interaction_mode_control(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::dark();
        let mode = self
            .selected_session()
            .map(|session| session.interaction_mode)
            .unwrap_or_default();
        let next_mode = if mode == InteractionMode::Plan {
            InteractionMode::Build
        } else {
            InteractionMode::Plan
        };
        let weak = cx.entity().downgrade();
        div()
            .id("interaction-mode")
            .h(px(24.0))
            .px(px(7.0))
            .rounded(px(6.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .cursor_default()
            .text_size(px(11.5))
            .line_height(px(14.0))
            .text_color(if mode == InteractionMode::Plan {
                theme.accent
            } else {
                theme.text_secondary
            })
            .hover(|element| element.bg(theme.overlay))
            .child(icon(
                if mode == InteractionMode::Plan {
                    "icons/list.svg"
                } else {
                    "icons/wrench.svg"
                },
                10.5,
                if mode == InteractionMode::Plan {
                    theme.accent
                } else {
                    theme.text_tertiary
                },
            ))
            .child(mode.label())
            .on_click(move |_, _, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.set_interaction_mode(next_mode, cx);
                });
            })
            .into_any_element()
    }

    fn render_composer(&self, _window: &Window, cx: &mut Context<Self>) -> Div {
        let theme = Theme::dark();
        let session = self.selected_session();
        let working = session
            .map(|session| {
                matches!(
                    session.status,
                    SessionStatus::Working | SessionStatus::Connecting | SessionStatus::Waiting
                )
            })
            .unwrap_or(false);
        let fresh_session = session
            .map(|session| session.messages.is_empty())
            .unwrap_or(false);
        let has_draft = !self.composer.read(cx).content().trim().is_empty();
        div().flex_none().px(px(20.0)).child(
            div()
                .w_full()
                .max_w(px(CONTENT_MAX_WIDTH))
                .mx_auto()
                .rounded(px(13.0))
                .border_1()
                .border_color(theme.border)
                .bg(theme.raised)
                .shadow(vec![BoxShadow {
                    color: hsla(0.0, 0.0, 0.0, 0.24),
                    offset: point(px(0.0), px(6.0)),
                    blur_radius: px(20.0),
                    spread_radius: px(-6.0),
                }])
                .p(px(10.0))
                .child(div().px(px(4.0)).pt(px(2.0)).child(self.composer.clone()))
                .child(
                    div()
                        .mt(px(8.0))
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .text_size(px(11.5))
                        .line_height(px(14.0))
                        .child(self.render_provider_model_control(fresh_session, cx))
                        .children(self.render_model_traits_control(cx))
                        .child(self.render_access_control(cx))
                        .child(self.render_interaction_mode_control(cx))
                        .child(div().flex_1())
                        .child(if working {
                            div()
                                .id("send-or-stop")
                                .w(px(26.0))
                                .h(px(26.0))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_default()
                                .bg(theme.overlay_strong)
                                .hover(|element| element.bg(theme.danger_soft))
                                .active(|element| element.opacity(0.8))
                                .child(icon("icons/stop.svg", 10.0, theme.text))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cancel_turn(cx);
                                }))
                        } else {
                            div()
                                .id("send-or-stop")
                                .w(px(26.0))
                                .h(px(26.0))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(if has_draft {
                                    theme.inverse
                                } else {
                                    theme.overlay_strong
                                })
                                .when(has_draft, |element| {
                                    element
                                        .cursor_default()
                                        .hover(|element| element.opacity(0.9))
                                        .active(|element| element.opacity(0.8))
                                })
                                .child(icon(
                                    "icons/arrow-up.svg",
                                    12.0,
                                    if has_draft {
                                        theme.on_inverse
                                    } else {
                                        theme.text_ghost
                                    },
                                ))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let prompt = this.composer.read(cx).content().trim().to_owned();
                                    if !prompt.is_empty() {
                                        this.composer.update(cx, |input, cx| input.clear(cx));
                                        this.submit_prompt(prompt, cx);
                                    }
                                }))
                        }),
                ),
        )
    }

    fn render_workspace_footer(&self) -> Div {
        let theme = Theme::dark();
        let path = self
            .selected_project()
            .map(|project| compact_path(&project.path))
            .unwrap_or_default();
        div()
            .flex_none()
            .px(px(20.0))
            .pb(px(8.0))
            .pt(px(4.0))
            .child(
                div()
                    .w_full()
                    .max_w(px(CONTENT_MAX_WIDTH))
                    .mx_auto()
                    .h(px(20.0))
                    // Left edge lines up with the composer card's inner icon
                    // column (10px card padding + 7px chip padding).
                    .pl(px(17.0))
                    .pr(px(10.0))
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .text_size(px(11.0))
                    .line_height(px(14.0))
                    .when_some(self.branch.clone(), |element, branch| {
                        element
                            .child(icon("icons/git-branch.svg", 10.5, theme.text_tertiary))
                            .child(
                                div()
                                    .text_color(theme.text_secondary)
                                    .child(SharedString::from(branch)),
                            )
                            .child(
                                div()
                                    .w(px(2.5))
                                    .h(px(2.5))
                                    .flex_none()
                                    .rounded_full()
                                    .bg(theme.text_ghost),
                            )
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(10.5))
                            .text_color(theme.text_ghost)
                            .child(SharedString::from(path)),
                    ),
            )
    }
}

impl Render for Waku {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
        let empty = self
            .selected_session()
            .map(|session| session.messages.is_empty())
            .unwrap_or(true);
        let permission = self.render_permission(cx);
        let toast = self.toast.clone();
        div()
            .key_context("Waku")
            .on_action(cx.listener(Self::new_session_action))
            .on_action(cx.listener(Self::toggle_sidebar_action))
            .on_action(cx.listener(Self::focus_composer_action))
            .on_action(cx.listener(Self::cancel_turn_action))
            .size_full()
            .flex()
            .bg(theme.canvas)
            .text_color(theme.text)
            .font_family(".SystemUIFont")
            .when(self.sidebar_visible, |root| {
                root.child(self.render_sidebar(cx))
            })
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .bg(theme.surface)
                    .when(self.sidebar_visible, |element| {
                        element.border_l_1().border_color(theme.border)
                    })
                    .child(self.render_header(cx))
                    .child(if empty {
                        self.render_empty_state(cx).into_any_element()
                    } else {
                        self.render_transcript(window, cx)
                    })
                    .children(permission)
                    .when_some(toast, |element, toast| {
                        element.child(
                            div()
                                .px(px(20.0))
                                .pb(px(8.0))
                                .flex()
                                .justify_center()
                                .child(
                                    div()
                                        .w_full()
                                        .max_w(px(CONTENT_MAX_WIDTH))
                                        .min_w_0()
                                        .px(px(12.0))
                                        .py(px(6.0))
                                        .rounded_full()
                                        .border_1()
                                        .border_color(theme.border_strong)
                                        .bg(theme.raised)
                                        .shadow_sm()
                                        .text_size(px(11.0))
                                        .text_color(theme.danger)
                                        .whitespace_normal()
                                        .child(SharedString::from(toast)),
                                ),
                        )
                    })
                    .when(self.selected_project().is_some(), |element| {
                        element
                            .child(self.render_composer(window, cx))
                            .child(self.render_workspace_footer())
                    }),
            )
    }
}

// ── Shared pieces ──────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum TranscriptRowKind {
    Message(usize),
    TurnBlock(usize),
}

fn transcript_anchor_end_space(viewport_height: Pixels, anchored_tail_height: Pixels) -> Pixels {
    (viewport_height - anchored_tail_height).max(Pixels::ZERO)
}

fn stabilized_transcript_anchor_end_space(
    viewport_height: Pixels,
    anchored_tail_height: Pixels,
    previous_end_space: Pixels,
    tail_is_unmeasured: bool,
) -> Pixels {
    let candidate = transcript_anchor_end_space(viewport_height, anchored_tail_height);
    if tail_is_unmeasured && previous_end_space > Pixels::ZERO {
        // Expansion needs the old (larger) spacer until its exact height is
        // known; collapse needs the new estimated (larger) spacer immediately.
        // Taking the maximum prevents GPUI's bottom-aligned list from ever
        // seeing an underfilled frame in either direction.
        previous_end_space.max(candidate)
    } else {
        candidate
    }
}

fn maintain_transcript_anchor(
    transcript_rows: &ListState,
    anchor_row: usize,
    anchor_following: bool,
    end_space: Pixels,
) -> bool {
    if !anchor_following || end_space <= Pixels::ZERO {
        return false;
    }

    // A bottom-aligned GPUI list represents its pinned tail as no logical
    // scroll offset. While a response row is being remeasured, the retained
    // end spacer and the newly expanded content briefly overflow together;
    // without an explicit item offset that overflow is taken from the top of
    // the user row. Reassert the turn anchor in the same layout pass.
    transcript_rows.scroll_to(ListOffset {
        item_ix: anchor_row,
        offset_in_item: Pixels::ZERO,
    });
    true
}

fn estimated_text_height(text: &str, characters_per_line: usize, line_height: f32) -> Pixels {
    let visual_lines = text
        .split('\n')
        .map(|line| {
            if line.trim().is_empty() {
                0.35
            } else {
                line.chars().count().max(1).div_ceil(characters_per_line) as f32
            }
        })
        .sum::<f32>()
        .max(1.0);
    px(visual_lines * line_height)
}

fn markdown_estimation_source(text: &str) -> (String, Pixels) {
    const DEFAULT_IMAGE_HEIGHT: f32 = 160.0;
    // Image sources can be multi-megabyte data URLs. The visible estimate is
    // usually much smaller, so do not duplicate that capacity or allocate a
    // second lowercase copy of the entire response.
    let mut visible = String::with_capacity(text.len().min(16 * 1024));
    let mut media_height = Pixels::ZERO;
    let mut offset = 0;

    while offset < text.len() {
        let remainder = &text[offset..];
        if starts_html_tag(remainder, "<details")
            && let Some(open_end) = remainder.find('>')
            && let Some((body_end, details_end)) = matching_details_end(remainder, open_end + 1)
        {
            let opening_tag = &remainder[..=open_end];
            let body = &remainder[open_end + 1..body_end];
            let source = if html_has_attribute(opening_tag, "open") {
                body
            } else if let Some(summary_start) = find_html_tag(body, "<summary") {
                let summary = &body[summary_start..];
                if let Some(tag_end) = summary.find('>') {
                    let content = &summary[tag_end + 1..];
                    find_ascii_case_insensitive(content, "</summary>")
                        .map_or("Details", |end| &content[..end])
                } else {
                    "Details"
                }
            } else {
                "Details"
            };
            let (details_text, details_media) = markdown_estimation_source(source);
            visible.push_str(&details_text);
            visible.push('\n');
            media_height += details_media;
            offset += details_end;
            continue;
        }
        if let Some(after_alt) = remainder.strip_prefix("![")
            && let Some(label_end) = after_alt.find("](")
        {
            let target = &after_alt[label_end + 2..];
            if let Some(target_end) = target.find(')') {
                media_height += px(DEFAULT_IMAGE_HEIGHT);
                visible.push('\n');
                offset += 2 + label_end + 2 + target_end + 1;
                continue;
            }
        }
        if starts_html_tag(remainder, "<img")
            && let Some(tag_end) = remainder.find('>')
        {
            let tag = &remainder[..=tag_end];
            let explicit_height = html_numeric_attribute(tag, "height")
                .map(|height| height.clamp(1.0, 720.0))
                .unwrap_or(DEFAULT_IMAGE_HEIGHT);
            media_height += px(explicit_height);
            visible.push('\n');
            offset += tag_end + 1;
            continue;
        }

        let Some(character) = remainder.chars().next() else {
            break;
        };
        visible.push(character);
        offset += character.len_utf8();
    }

    (visible, media_height)
}

fn find_ascii_case_insensitive(source: &str, needle: &str) -> Option<usize> {
    source
        .as_bytes()
        .windows(needle.len())
        .position(|candidate| candidate.eq_ignore_ascii_case(needle.as_bytes()))
}

fn starts_html_tag(source: &str, name: &str) -> bool {
    source
        .get(..name.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(name))
        && source[name.len()..]
            .chars()
            .next()
            .is_some_and(|character| {
                character.is_ascii_whitespace() || character == '>' || character == '/'
            })
}

fn find_html_tag(source: &str, name: &str) -> Option<usize> {
    let mut cursor = 0;
    loop {
        let index = cursor + find_ascii_case_insensitive(&source[cursor..], name)?;
        if starts_html_tag(&source[index..], name) {
            return Some(index);
        }
        cursor = index + name.len();
    }
}

fn matching_details_end(source: &str, body_start: usize) -> Option<(usize, usize)> {
    let mut depth = 1usize;
    let mut cursor = body_start;
    while cursor < source.len() {
        let next_open = find_html_tag(&source[cursor..], "<details").map(|index| cursor + index);
        let next_close = find_ascii_case_insensitive(&source[cursor..], "</details>")
            .map(|index| cursor + index);
        match (next_open, next_close) {
            (_, None) => return None,
            (Some(open), Some(close)) if open < close => {
                depth += 1;
                cursor = open + "<details".len();
            }
            (_, Some(close)) => {
                depth -= 1;
                if depth == 0 {
                    return Some((close, close + "</details>".len()));
                }
                cursor = close + "</details>".len();
            }
        }
    }
    None
}

fn html_has_attribute(tag: &str, name: &str) -> bool {
    tag.split(|character: char| {
        character.is_ascii_whitespace() || character == '>' || character == '/'
    })
    .any(|attribute| {
        attribute.eq_ignore_ascii_case(name)
            || attribute
                .split_once('=')
                .is_some_and(|(key, _)| key.eq_ignore_ascii_case(name))
    })
}

fn html_numeric_attribute(tag: &str, name: &str) -> Option<f32> {
    let mut cursor = 0;
    let start = loop {
        let index = cursor + find_ascii_case_insensitive(&tag[cursor..], name)?;
        let before = tag[..index].chars().next_back();
        let after = tag[index + name.len()..].chars().next();
        if before.is_some_and(|character| character.is_ascii_whitespace())
            && after.is_some_and(|character| character.is_ascii_whitespace() || character == '=')
        {
            break index + name.len();
        }
        cursor = index + name.len();
    };
    let value = tag[start..].trim_start().strip_prefix('=')?.trim_start();
    let value = value
        .strip_prefix(['\'', '"'])
        .unwrap_or(value)
        .split(|character: char| {
            character == '\''
                || character == '"'
                || character.is_ascii_whitespace()
                || character == '>'
        })
        .next()?;
    value.trim_end_matches("px").parse().ok()
}

fn estimated_message_height(
    message: &Message,
    content_width: Pixels,
    checkpoint_visible: bool,
) -> Pixels {
    let assistant_columns = if content_width > Pixels::ZERO {
        (content_width / px(7.25)).max(20.0) as usize
    } else {
        88
    };
    let user_width = content_width.min(px(540.0));
    let user_columns = if user_width > Pixels::ZERO {
        (user_width / px(7.5)).max(20.0) as usize
    } else {
        72
    };
    match message.role {
        MessageRole::User => estimated_text_height(&message.content, user_columns, 20.0) + px(16.0),
        MessageRole::Assistant => {
            let checkpoint_height = if checkpoint_visible {
                px(28.0)
            } else {
                Pixels::ZERO
            };
            let (visible_source, media_height) = markdown_estimation_source(&message.content);
            estimated_text_height(&visible_source, assistant_columns, 21.0)
                + media_height
                + px(8.0)
                + checkpoint_height
        }
        MessageRole::System => {
            estimated_text_height(&message.content, assistant_columns, 16.0) + px(8.0)
        }
    }
}

fn estimated_transcript_row_padding(
    row_index: usize,
    row_count: usize,
    starts_followup_turn: bool,
) -> Pixels {
    let top = if row_index == 0 {
        px(22.0)
    } else if starts_followup_turn {
        px(FOLLOWUP_TURN_TOP_GAP)
    } else {
        px(8.0)
    };
    let bottom = if row_index + 1 == row_count {
        px(22.0)
    } else {
        px(8.0)
    };
    top + bottom
}

/// Interleave live turn blocks at the exact message boundary where their
/// provider events arrived. `anchors[n] == 2` means block `n` renders after
/// messages 0 and 1, before message 2.
fn transcript_row_kinds(message_count: usize, anchors: &[usize]) -> Vec<TranscriptRowKind> {
    let mut blocks_after = vec![Vec::new(); message_count + 1];
    for (block_index, anchor) in anchors.iter().copied().enumerate() {
        blocks_after[anchor.min(message_count)].push(block_index);
    }
    let mut rows = Vec::with_capacity(message_count + anchors.len());
    rows.extend(
        blocks_after[0]
            .iter()
            .copied()
            .map(TranscriptRowKind::TurnBlock),
    );
    for message_index in 0..message_count {
        rows.push(TranscriptRowKind::Message(message_index));
        rows.extend(
            blocks_after[message_index + 1]
                .iter()
                .copied()
                .map(TranscriptRowKind::TurnBlock),
        );
    }
    rows
}

fn message_starts_followup_turn(messages: &[Message], message_index: usize) -> bool {
    messages
        .get(message_index)
        .is_some_and(|message| message.role == MessageRole::User)
        && messages[..message_index]
            .iter()
            .any(|message| message.role == MessageRole::User)
}

fn stream_delta_kind(event: &DriverEvent) -> Option<StreamDeltaKind> {
    match event {
        DriverEvent::TextDelta(_) => Some(StreamDeltaKind::Text),
        DriverEvent::ReasoningDelta(_) => Some(StreamDeltaKind::Reasoning),
        _ => None,
    }
}

fn stream_delta_text(event: &DriverEvent, kind: StreamDeltaKind) -> Option<&str> {
    match (kind, event) {
        (StreamDeltaKind::Text, DriverEvent::TextDelta(text))
        | (StreamDeltaKind::Reasoning, DriverEvent::ReasoningDelta(text)) => Some(text),
        _ => None,
    }
}

fn stream_frame_budget(backlog: usize) -> usize {
    backlog
        .div_ceil(STREAM_CATCH_UP_FRAMES)
        .clamp(
            STREAM_MIN_GRAPHEMES_PER_FRAME,
            STREAM_MAX_GRAPHEMES_PER_FRAME,
        )
        .min(backlog)
}

/// Pop one display-sized chunk while retaining the provider's event order.
///
/// Adjacent deltas of the same kind are coalesced. Large deltas are split on
/// grapheme and line boundaries, so a provider that emits its whole answer in
/// one event still gets the same progressive presentation as token streams.
fn pop_stream_chunk(
    events: &mut VecDeque<DriverEvent>,
    kind: StreamDeltaKind,
) -> Option<DriverEvent> {
    let backlog = events
        .iter()
        .map_while(|event| stream_delta_text(event, kind))
        .map(|text| text.graphemes(true).count())
        .sum();
    if backlog == 0 {
        return events.pop_front();
    }

    let mut remaining_budget = stream_frame_budget(backlog);
    let mut chunk = String::new();
    while remaining_budget > 0 {
        let Some(text) = events.front_mut().and_then(|event| match (kind, event) {
            (StreamDeltaKind::Text, DriverEvent::TextDelta(text))
            | (StreamDeltaKind::Reasoning, DriverEvent::ReasoningDelta(text)) => Some(text),
            _ => None,
        }) else {
            break;
        };

        let (prefix, graphemes) = take_stream_prefix(text, remaining_budget);
        let reached_line_boundary = prefix.ends_with('\n');
        chunk.push_str(&prefix);
        remaining_budget = remaining_budget.saturating_sub(graphemes);
        if text.is_empty() {
            events.pop_front();
        }
        if reached_line_boundary {
            break;
        }
    }

    match kind {
        StreamDeltaKind::Text => Some(DriverEvent::TextDelta(chunk)),
        StreamDeltaKind::Reasoning => Some(DriverEvent::ReasoningDelta(chunk)),
    }
}

fn take_stream_prefix(text: &mut String, budget: usize) -> (String, usize) {
    if text.is_empty() || budget == 0 {
        return (String::new(), 0);
    }

    let mut count = 0;
    let mut end = text.len();
    for (start, grapheme) in text.grapheme_indices(true) {
        count += 1;
        end = start + grapheme.len();
        if grapheme == "\n" || count == budget {
            break;
        }
    }

    let remainder = text.split_off(end);
    (std::mem::replace(text, remainder), count)
}

fn pulse_dot(id: impl Into<SharedString>, size: f32, color: Hsla) -> AnyElement {
    div()
        .w(px(size))
        .h(px(size))
        .flex_none()
        .rounded_full()
        .bg(color)
        .with_animation(
            id.into(),
            Animation::new(Duration::from_millis(1600))
                .repeat()
                .with_easing(pulsating_between(0.3, 1.0)),
            |element, delta| element.opacity(delta),
        )
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn render_message(
    theme: &Theme,
    message: &Message,
    checkpoint_action: Option<CheckpointAction>,
    session_id: Uuid,
    transcript_resize_tx: crossbeam_channel::Sender<TranscriptMarkdownResize>,
    transcript_layout_width: Pixels,
    transcript_rows: ListState,
    transcript_viewport: TextViewScrollViewport,
    text_state: Entity<TextViewState>,
    waku: gpui::WeakEntity<Waku>,
    composer: Entity<ComposerInput>,
    cx: &mut App,
) -> AnyElement {
    let content = message.content.clone();
    let message_id = message.id;
    let role = message.role;
    let code = fenced_code(&content);
    let menu_content = content.clone();
    let element = match role {
        MessageRole::User => div().w_full().flex().justify_end().child(
            div()
                .max_w(px(540.0))
                .rounded(px(12.0))
                .bg(theme.raised)
                .px(px(12.0))
                .py(px(8.0))
                .text_size(px(14.0))
                .line_height(px(20.0))
                .text_color(theme.text)
                .whitespace_normal()
                .child(
                    selectable_plain_text(
                        SharedString::from(format!("message-{message_id}-user")),
                        &content,
                        text_state,
                        cx,
                    )
                    .selection_scroll_handle(&transcript_rows)
                    .block_viewport(transcript_viewport),
                ),
        ),
        MessageRole::Assistant => {
            let resize_tx = transcript_resize_tx.clone();
            let resize_waku = waku.clone();
            let mut column = div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .py(px(4.0))
                .text_size(px(13.5))
                .line_height(px(21.0))
                .text_color(theme.text)
                .child(
                    TextView::markdown_with_state(
                        SharedString::from(format!("message-{message_id}-assistant")),
                        content,
                        text_state,
                        cx,
                    )
                    .update_delay(STREAM_MARKDOWN_DELAY)
                    .style(assistant_markdown_style(theme))
                    .selectable(true)
                    .selection_scroll_handle(&transcript_rows)
                    .block_viewport(transcript_viewport)
                    .block_layout_width(transcript_layout_width)
                    .on_block_resize(move |resize: TextViewBlockResize, cx| {
                        let _ = resize_tx.send(TranscriptMarkdownResize {
                            session_id,
                            message_id,
                            delta: resize.delta,
                            anchor_delta: if resize.above_viewport {
                                resize.delta
                            } else {
                                Pixels::ZERO
                            },
                        });
                        if let Some(waku) = resize_waku.upgrade() {
                            cx.notify(waku.entity_id());
                        }
                    })
                    .w_full()
                    .cursor_text(),
                );
            if message.streaming {
                column = column.child(pulse_dot(
                    format!("stream-{}", message.id),
                    6.0,
                    theme.accent,
                ));
            }
            if let Some(action) = checkpoint_action {
                let checkpoint_label = if action.file_count == 0 {
                    format!("Checkpoint {} · no file changes", action.turn_count)
                } else {
                    format!(
                        "Checkpoint {} · {} file(s)",
                        action.turn_count, action.file_count
                    )
                };
                let weak = waku.clone();
                column = column.child(
                    div()
                        .mt(px(8.0))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .text_size(px(10.5))
                        .line_height(px(16.0))
                        .text_color(theme.text_tertiary)
                        .child(icon("icons/check.svg", 10.0, theme.text_tertiary))
                        .child(SharedString::from(checkpoint_label))
                        .when(action.can_revert, |element| {
                            element.child(
                                div()
                                    .id(SharedString::from(format!(
                                        "revert-checkpoint-{}-{}",
                                        action.session_id, action.turn_count
                                    )))
                                    .ml(px(2.0))
                                    .px(px(7.0))
                                    .py(px(2.0))
                                    .rounded(px(5.0))
                                    .cursor_default()
                                    .text_color(if action.confirmed {
                                        theme.danger
                                    } else {
                                        theme.text_secondary
                                    })
                                    .bg(if action.confirmed {
                                        theme.danger_soft
                                    } else {
                                        theme.overlay
                                    })
                                    .hover(|element| element.bg(theme.overlay_strong))
                                    .child(if action.confirmed {
                                        "Confirm revert"
                                    } else {
                                        "Revert"
                                    })
                                    .on_click(move |_, _, cx| {
                                        let _ = weak.update(cx, |this, cx| {
                                            this.request_checkpoint_revert(
                                                action.session_id,
                                                action.turn_count,
                                                cx,
                                            );
                                        });
                                    }),
                            )
                        }),
                );
            }
            column
        }
        MessageRole::System => div().w_full().flex().justify_center().child(
            div()
                .px(px(10.0))
                .py(px(4.0))
                .rounded_full()
                .bg(theme.overlay)
                .text_size(px(11.0))
                .line_height(px(16.0))
                .text_color(theme.text_tertiary)
                .child(
                    selectable_plain_text(
                        SharedString::from(format!("message-{message_id}-system")),
                        &content,
                        text_state,
                        cx,
                    )
                    .selection_scroll_handle(&transcript_rows)
                    .block_viewport(transcript_viewport),
                ),
        ),
    };

    element
        .id(message_id)
        .context_menu_with_id(
            SharedString::from(format!("message-context-menu-{message_id}")),
            move |menu, window, cx| {
                let copy_content = menu_content.clone();
                let mut menu =
                    preserve_composer_focus_for_context_menu(&composer, menu, window, cx)
                        .min_w(px(170.0))
                        .item(
                            PopupMenuItem::new("Copy Message").on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    copy_content.clone(),
                                ));
                            }),
                        );

                if role == MessageRole::User {
                    let composer = composer.clone();
                    let edit_content = menu_content.clone();
                    menu = menu.item(PopupMenuItem::new("Edit in Composer").on_click(
                        move |_, window, cx| {
                            composer.update(cx, |composer, cx| {
                                composer.set_content(edit_content.clone(), cx);
                            });
                            window.focus(&composer.read(cx).focus());
                        },
                    ));
                }

                if let Some(code) = code.clone() {
                    menu = menu.item(PopupMenuItem::new("Copy Code").on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                    }));
                }

                if let Some(action) = checkpoint_action.filter(|action| action.can_revert) {
                    let weak = waku.clone();
                    menu = menu.item(
                        PopupMenuItem::new(if action.confirmed {
                            "Confirm Revert to Checkpoint"
                        } else {
                            "Revert to Checkpoint"
                        })
                        .on_click(move |_, _, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                this.request_checkpoint_revert(
                                    action.session_id,
                                    action.turn_count,
                                    cx,
                                );
                            });
                        }),
                    );
                }

                menu
            },
        )
        .into_any_element()
}

fn assistant_markdown_style(theme: &Theme) -> TextViewStyle {
    TextViewStyle::default()
        .paragraph_gap(rems(0.75))
        .heading_font_size(|level, base| match level {
            1 => base * 1.5,
            2 => base * 1.3,
            3 => base * 1.15,
            4 => base * 1.05,
            _ => base,
        })
        .code_block(
            StyleRefinement::default()
                .bg(theme.inset)
                .border_1()
                .border_color(theme.border_strong)
                .rounded(px(8.0))
                .p(px(12.0))
                .text_size(px(12.0)),
        )
}

fn selectable_plain_text(
    id: impl Into<gpui::ElementId>,
    content: &str,
    state: Entity<TextViewState>,
    cx: &mut App,
) -> TextView {
    let html = if content.is_empty() {
        "<p></p>".to_owned()
    } else {
        content
            .split('\n')
            .map(|line| format!("<p>{}</p>", escape_html(line)))
            .collect::<String>()
    };
    TextView::html_with_state(id, html, state, cx)
        .style(TextViewStyle::default().paragraph_gap(rems(0.0)))
        .selectable(true)
        .w_full()
        .cursor_text()
}

fn escape_html(content: &str) -> String {
    content
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn fenced_code(content: &str) -> Option<String> {
    let mut code_blocks = Vec::new();
    let mut segments = content.split("```");
    let _ = segments.next();
    while let Some(fenced) = segments.next() {
        let (language, code) = fenced
            .split_once('\n')
            .map(|(language, code)| (language.trim(), code))
            .unwrap_or(("", fenced));
        let code = if language.is_empty() && !fenced.contains('\n') {
            fenced
        } else {
            code
        };
        if !code.trim().is_empty() {
            code_blocks.push(code.trim_end().to_owned());
        }
        let _ = segments.next();
    }
    (!code_blocks.is_empty()).then(|| code_blocks.join("\n\n"))
}

fn activity_summary(activities: &[ActivityItem]) -> String {
    let mut counts: Vec<(crate::model::ActivityKind, usize)> = Vec::new();
    for activity in activities {
        if let Some(entry) = counts.iter_mut().find(|(kind, _)| *kind == activity.kind) {
            entry.1 += 1;
        } else {
            counts.push((activity.kind, 1));
        }
    }
    let parts = counts
        .into_iter()
        .map(|(kind, count)| {
            let (singular, plural) = activity_noun(kind);
            format!("{count} {}", if count == 1 { singular } else { plural })
        })
        .collect::<Vec<_>>();
    let running = activities.iter().any(|activity| !activity.complete);
    format!(
        "{} {}",
        if running { "Running" } else { "Ran" },
        parts.join(" · ")
    )
}

fn append_text_delta_to_session(
    sessions: &mut [AgentSession],
    session_id: Uuid,
    continuing: bool,
    delta: String,
) {
    let Some(session) = sessions.iter_mut().find(|session| session.id == session_id) else {
        return;
    };
    if !continuing {
        for message in &mut session.messages {
            if message.role == MessageRole::Assistant && message.streaming {
                message.streaming = false;
            }
        }
    }
    let existing = continuing.then(|| {
        session
            .messages
            .iter_mut()
            .rev()
            .find(|message| message.role == MessageRole::Assistant && message.streaming)
    });
    if let Some(Some(message)) = existing {
        message.content.push_str(&delta);
    } else {
        let mut message = session
            .active_turn_id()
            .map(|turn_id| Message::new_for_turn(MessageRole::Assistant, delta.clone(), turn_id))
            .unwrap_or_else(|| Message::new(MessageRole::Assistant, delta));
        message.streaming = true;
        session.messages.push(message);
    }
    session.updated_at = unix_time();
}

fn git_branch(path: &std::path::Path) -> Option<String> {
    let output = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(path)
        .output()
        .ok()?;
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!branch.is_empty()).then_some(branch)
}

#[cfg(test)]
mod tests {
    use super::{
        StableListScrollbarHandle, StreamDeltaKind, TranscriptRowKind::*,
        append_text_delta_to_session, escape_html, estimated_message_height, estimated_text_height,
        fenced_code, maintain_transcript_anchor, markdown_estimation_source,
        message_starts_followup_turn, pop_stream_chunk, scale_scrollbar_offset,
        scroll_top_after_row_invalidation, stabilized_transcript_anchor_end_space,
        take_stream_prefix, transcript_anchor_end_space, transcript_row_kinds,
    };
    use crate::model::{
        ActivityKind, AgentSession, DriverEvent, Message, MessageRole, ProviderKind, SessionStatus,
    };
    use std::{cell::Cell, collections::VecDeque, rc::Rc};

    #[test]
    fn stable_scrollbar_maps_live_offsets_without_changing_progress() {
        let actual_max = gpui::size(gpui::px(0.0), gpui::px(600.0));
        let stable_max = gpui::size(gpui::px(0.0), gpui::px(2_400.0));

        assert_eq!(
            scale_scrollbar_offset(
                gpui::point(gpui::px(0.0), gpui::px(-300.0)),
                actual_max,
                stable_max,
            ),
            gpui::point(gpui::px(0.0), gpui::px(-1_200.0))
        );
        assert_eq!(
            scale_scrollbar_offset(
                gpui::point(gpui::px(0.0), gpui::px(-2_400.0)),
                stable_max,
                actual_max,
            ),
            gpui::point(gpui::px(0.0), gpui::px(-600.0))
        );
    }

    #[test]
    fn stable_scrollbar_freezes_its_document_height_during_a_drag() {
        use gpui_component::scroll::ScrollbarHandle as _;

        let rows = gpui::ListState::new(0, gpui::ListAlignment::Bottom, gpui::px(0.0));
        let estimated = Rc::new(Cell::new(gpui::px(1_000.0)));
        let anchor_end_space = Rc::new(Cell::new(gpui::px(300.0)));
        let anchor_following = Rc::new(Cell::new(true));
        let drag_estimate = Rc::new(Cell::new(None));
        let is_scrolled = Rc::new(Cell::new(false));
        let handle = StableListScrollbarHandle::new(
            &rows,
            &estimated,
            &anchor_end_space,
            &anchor_following,
            &drag_estimate,
            &is_scrolled,
        );

        handle.start_drag();
        estimated.set(gpui::px(2_000.0));
        anchor_end_space.set(gpui::px(0.0));
        assert_eq!(handle.content_size().height, gpui::px(1_300.0));
        handle.end_drag();
        assert_eq!(handle.content_size().height, gpui::px(2_000.0));
    }

    #[test]
    fn anchor_end_space_keeps_a_short_new_turn_at_the_viewport_top() {
        assert_eq!(
            transcript_anchor_end_space(gpui::px(700.0), gpui::px(180.0)),
            gpui::px(520.0)
        );
        assert_eq!(
            transcript_anchor_end_space(gpui::px(700.0), gpui::px(900.0)),
            gpui::px(0.0)
        );
    }

    #[test]
    fn anchor_end_space_waits_for_exact_expanded_row_measurement() {
        assert_eq!(
            stabilized_transcript_anchor_end_space(
                gpui::px(700.0),
                gpui::px(260.0),
                gpui::px(520.0),
                true,
            ),
            gpui::px(520.0)
        );
        assert_eq!(
            stabilized_transcript_anchor_end_space(
                gpui::px(700.0),
                gpui::px(260.0),
                gpui::px(520.0),
                false,
            ),
            gpui::px(440.0)
        );
        assert_eq!(
            stabilized_transcript_anchor_end_space(
                gpui::px(700.0),
                gpui::px(180.0),
                gpui::px(440.0),
                true,
            ),
            gpui::px(520.0)
        );
    }

    #[test]
    fn pending_expansion_reasserts_the_user_message_anchor() {
        let rows = gpui::ListState::new(3, gpui::ListAlignment::Bottom, gpui::px(0.0));
        rows.scroll_to(gpui::ListOffset {
            item_ix: 0,
            offset_in_item: gpui::px(42.0),
        });

        assert!(maintain_transcript_anchor(&rows, 0, true, gpui::px(320.0),));
        let anchored = rows.logical_scroll_top();
        assert_eq!(anchored.item_ix, 0);
        assert_eq!(anchored.offset_in_item, gpui::Pixels::ZERO);
        assert!(!maintain_transcript_anchor(
            &rows,
            0,
            true,
            gpui::Pixels::ZERO,
        ));
    }

    #[test]
    fn row_invalidation_preserves_the_intra_message_anchor() {
        let scroll_top = gpui::ListOffset {
            item_ix: 4,
            offset_in_item: gpui::px(320.0),
        };
        let anchored = scroll_top_after_row_invalidation(scroll_top, 4..5, gpui::px(80.0))
            .expect("the invalidated row contains the scroll top");
        assert_eq!(anchored.item_ix, 4);
        assert_eq!(anchored.offset_in_item, gpui::px(400.0));
        assert!(scroll_top_after_row_invalidation(scroll_top, 5..6, gpui::px(80.0)).is_none());
    }

    #[test]
    fn transcript_estimates_count_explicit_markdown_lines() {
        let markdown = (1..=200)
            .map(|line| format!("{line}. A short sentence."))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(estimated_text_height(&markdown, 88, 21.0) >= gpui::px(4_200.0));
    }

    #[test]
    fn markdown_estimates_reserve_images_without_counting_long_sources_as_text() {
        let markdown =
            "Before\n\n![preview](data:image/png;base64,AAAAAAAAAAAAAAAAAAAAAAAA)\n\nAfter";
        let (visible, media_height) = markdown_estimation_source(markdown);
        assert!(!visible.contains("base64"));
        assert_eq!(media_height, gpui::px(160.0));

        let message = Message::new(MessageRole::Assistant, markdown);
        assert!(estimated_message_height(&message, gpui::px(720.0), false) >= gpui::px(200.0));
    }

    #[test]
    fn assistant_estimate_reserves_only_a_visible_checkpoint() {
        let message = Message::new(MessageRole::Assistant, "A short response.");
        let without_checkpoint = estimated_message_height(&message, gpui::px(720.0), false);
        let with_checkpoint = estimated_message_height(&message, gpui::px(720.0), true);

        assert_eq!(with_checkpoint - without_checkpoint, gpui::px(28.0));
    }

    #[test]
    fn markdown_estimates_only_visible_disclosure_content() {
        let closed = "<details><summary>More</summary>hidden ![image](x.png)</details>";
        let (visible, media_height) = markdown_estimation_source(closed);
        assert!(visible.contains("More"));
        assert!(!visible.contains("hidden"));
        assert_eq!(media_height, gpui::Pixels::ZERO);

        let open = "<details open><summary>More</summary>visible ![image](x.png)</details>";
        let (visible, media_height) = markdown_estimation_source(open);
        assert!(visible.contains("visible"));
        assert_eq!(media_height, gpui::px(160.0));

        let nested = "<DETAILS OPEN><SUMMARY>Outer</SUMMARY><details><summary>Inner</summary>hidden</details><IMG HEIGHT='245' src='x'></DETAILS>";
        let (visible, media_height) = markdown_estimation_source(nested);
        assert!(visible.contains("Outer"));
        assert!(visible.contains("Inner"));
        assert!(!visible.contains("hidden"));
        assert_eq!(media_height, gpui::px(245.0));

        let (visible, media_height) =
            markdown_estimation_source("<details-panel>ordinary text</details-panel>");
        assert!(visible.contains("ordinary text"));
        assert_eq!(media_height, gpui::Pixels::ZERO);
    }

    #[test]
    fn plain_message_html_is_escaped() {
        assert_eq!(
            escape_html("<tag a='b'>&\""),
            "&lt;tag a=&#39;b&#39;&gt;&amp;&quot;"
        );
    }

    #[test]
    fn only_later_user_messages_start_followup_turns() {
        let messages = vec![
            Message::new(MessageRole::User, "first"),
            Message::new(MessageRole::Assistant, "answer"),
            Message::new(MessageRole::User, "follow-up"),
            Message::new(MessageRole::Assistant, "answer"),
        ];
        assert!(!message_starts_followup_turn(&messages, 0));
        assert!(!message_starts_followup_turn(&messages, 1));
        assert!(message_starts_followup_turn(&messages, 2));
        assert!(!message_starts_followup_turn(&messages, 3));
    }

    #[test]
    fn fenced_code_collects_all_blocks_without_languages() {
        let markdown = "Before\n```rust\nfn main() {}\n```\nAfter\n```\ncargo test\n```";
        assert_eq!(
            fenced_code(markdown).as_deref(),
            Some("fn main() {}\n\ncargo test")
        );
        assert_eq!(fenced_code("No code here"), None);
    }

    #[test]
    fn stream_prefix_stops_at_lines_without_splitting_graphemes() {
        let mut text = "hello 👋🏽\nnext line".to_owned();
        let (first, count) = take_stream_prefix(&mut text, 100);
        assert_eq!(first, "hello 👋🏽\n");
        assert_eq!(count, 8);
        assert_eq!(text, "next line");

        let mut emoji = "👨‍👩‍👧‍👦x".to_owned();
        let (first, count) = take_stream_prefix(&mut emoji, 1);
        assert_eq!(first, "👨‍👩‍👧‍👦");
        assert_eq!(count, 1);
        assert_eq!(emoji, "x");
    }

    #[test]
    fn stream_chunks_coalesce_deltas_and_preserve_event_order() {
        let mut events = VecDeque::from([
            DriverEvent::TextDelta("first ".into()),
            DriverEvent::TextDelta("line\nsecond line".into()),
            DriverEvent::Activity {
                id: None,
                kind: ActivityKind::Tool,
                title: "Tool".into(),
                detail: None,
                complete: true,
            },
            DriverEvent::TextDelta("after tool".into()),
        ]);

        assert!(matches!(
            pop_stream_chunk(&mut events, StreamDeltaKind::Text),
            Some(DriverEvent::TextDelta(text)) if text == "first line\n"
        ));
        assert!(matches!(
            events.front(),
            Some(DriverEvent::TextDelta(text)) if text == "second line"
        ));

        assert!(matches!(
            pop_stream_chunk(&mut events, StreamDeltaKind::Text),
            Some(DriverEvent::TextDelta(text)) if text == "second line"
        ));
        assert!(matches!(events.front(), Some(DriverEvent::Activity { .. })));
    }

    #[test]
    fn stream_parts_keep_targeting_the_running_session_after_selection_changes() {
        let project_id = uuid::Uuid::new_v4();
        let mut running = AgentSession::new(project_id, ProviderKind::Codex);
        running.begin_turn("background task");
        running.status = SessionStatus::Working;
        let running_id = running.id;
        let visible = AgentSession::new(project_id, ProviderKind::Claude);
        let visible_id = visible.id;
        let mut sessions = vec![running, visible];

        append_text_delta_to_session(&mut sessions, running_id, false, "first".into());
        // Navigation changes only which task is rendered. The runtime keeps
        // emitting with its own task ID while another task is visible.
        let selected_session = visible_id;
        append_text_delta_to_session(&mut sessions, running_id, true, " second".into());

        assert_eq!(selected_session, visible_id);
        assert_eq!(sessions[0].messages[1].content, "first second");
        assert!(sessions[0].messages[1].streaming);
        assert!(sessions[1].messages.is_empty());
    }

    #[test]
    fn turn_blocks_keep_their_message_boundaries() {
        // user, assistant text, tool row, assistant text, reasoning row,
        // assistant text
        let rows = transcript_row_kinds(4, &[2, 3]);
        assert_eq!(
            rows,
            vec![
                Message(0),
                Message(1),
                TurnBlock(0),
                Message(2),
                TurnBlock(1),
                Message(3)
            ]
        );
    }

    #[test]
    fn blocks_follow_the_latest_message_without_a_reply() {
        let rows = transcript_row_kinds(2, &[2]);
        assert_eq!(rows, vec![Message(0), Message(1), TurnBlock(0)]);
    }

    #[test]
    fn plain_transcript_maps_one_to_one() {
        let rows = transcript_row_kinds(4, &[]);
        assert_eq!(rows, vec![Message(0), Message(1), Message(2), Message(3)]);
    }

    #[test]
    fn multiple_blocks_at_one_boundary_preserve_event_order() {
        let rows = transcript_row_kinds(2, &[1, 1]);
        assert_eq!(
            rows,
            vec![Message(0), TurnBlock(0), TurnBlock(1), Message(1)]
        );
    }
}
