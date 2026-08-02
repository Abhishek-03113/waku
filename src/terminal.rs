use std::borrow::Cow;
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point as TerminalPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::tty::{self, Shell};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use anyhow::{Context as _, Result};
use crossbeam_channel::{Receiver, Sender, unbounded};
use gpui::{
    App, AppContext, Bounds, ClipboardItem, Context, FocusHandle, Focusable, FontStyle, FontWeight,
    Hsla, InteractiveElement, IntoElement, KeyDownEvent, Keystroke, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Render, ScrollDelta,
    ScrollWheelEvent, SharedString, StrikethroughStyle, Styled, StyledText, Subscription, Task,
    TextRun, UnderlineStyle, Window, canvas, div, font, px, rgb,
};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::theme::ActiveTheme as _;
use parking_lot::Mutex;

use crate::persistence::DEFAULT_RIGHT_PANEL_WIDTH;
use crate::theme::Theme;

const TERMINAL_CELL_WIDTH: f32 = 7.2;
const TERMINAL_CELL_HEIGHT: f32 = 16.0;
const TERMINAL_FONT_SIZE: f32 = 11.5;
const TERMINAL_PADDING_X: f32 = 10.0;
const TERMINAL_PADDING_Y: f32 = 8.0;
const TERMINAL_TOOLBAR_HEIGHT: f32 = 34.0;
const TERMINAL_MIN_COLUMNS: usize = 20;
const TERMINAL_MIN_ROWS: usize = 8;
const TERMINAL_SCROLLBACK_LINES: usize = 10_000;
const TERMINAL_CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(500);
const TERMINAL_CURSOR_BLINK_PAUSE: Duration = Duration::from_millis(300);

enum TerminalUiEvent {
    Title(String),
    ResetTitle,
    ClipboardStore(String),
    ClipboardLoad(Arc<dyn Fn(&str) -> String + Send + Sync>),
    Exited,
}

#[derive(Clone)]
struct TerminalEventProxy {
    dirty: Arc<AtomicBool>,
    sender: Arc<OnceLock<EventLoopSender>>,
    ui_events: Sender<TerminalUiEvent>,
    window_size: Arc<Mutex<WindowSize>>,
}

impl TerminalEventProxy {
    fn write_pty(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        if let Some(sender) = self.sender.get() {
            let _ = sender.send(Msg::Input(bytes.into()));
        }
    }
}

impl EventListener for TerminalEventProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::Wakeup | Event::MouseCursorDirty | Event::CursorBlinkingChange => {
                self.dirty.store(true, Ordering::Release);
            }
            Event::Title(title) => {
                let _ = self.ui_events.send(TerminalUiEvent::Title(title));
            }
            Event::ResetTitle => {
                let _ = self.ui_events.send(TerminalUiEvent::ResetTitle);
            }
            Event::ClipboardStore(_, text) => {
                let _ = self.ui_events.send(TerminalUiEvent::ClipboardStore(text));
            }
            Event::ClipboardLoad(_, formatter) => {
                let _ = self
                    .ui_events
                    .send(TerminalUiEvent::ClipboardLoad(formatter));
            }
            Event::PtyWrite(text) => self.write_pty(text.into_bytes()),
            Event::ColorRequest(index, formatter) => {
                self.write_pty(formatter(terminal_rgb(index)).into_bytes());
            }
            Event::TextAreaSizeRequest(formatter) => {
                self.write_pty(formatter(*self.window_size.lock()).into_bytes());
            }
            Event::Bell => {}
            Event::Exit | Event::ChildExit(_) => {
                let _ = self.ui_events.send(TerminalUiEvent::Exited);
                self.dirty.store(true, Ordering::Release);
            }
        }
    }
}

struct TerminalDimensions {
    columns: usize,
    rows: usize,
}

impl Dimensions for TerminalDimensions {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

struct TerminalSession {
    term: Arc<FairMutex<Term<TerminalEventProxy>>>,
    sender: EventLoopSender,
    dirty: Arc<AtomicBool>,
    ui_events: Receiver<TerminalUiEvent>,
    window_size: Arc<Mutex<WindowSize>>,
    grid_size: (usize, usize),
}

impl TerminalSession {
    fn new(working_directory: &Path, columns: usize, rows: usize) -> Result<Self> {
        let columns = columns.max(TERMINAL_MIN_COLUMNS);
        let rows = rows.max(TERMINAL_MIN_ROWS);
        let window_size = WindowSize {
            num_lines: rows.min(u16::MAX as usize) as u16,
            num_cols: columns.min(u16::MAX as usize) as u16,
            cell_width: TERMINAL_CELL_WIDTH.round() as u16,
            cell_height: TERMINAL_CELL_HEIGHT.round() as u16,
        };
        let shared_window_size = Arc::new(Mutex::new(window_size));
        let dirty = Arc::new(AtomicBool::new(true));
        let sender_slot = Arc::new(OnceLock::new());
        let (ui_event_tx, ui_events) = unbounded();
        let proxy = TerminalEventProxy {
            dirty: dirty.clone(),
            sender: sender_slot.clone(),
            ui_events: ui_event_tx,
            window_size: shared_window_size.clone(),
        };

        let config = Config {
            scrolling_history: TERMINAL_SCROLLBACK_LINES,
            ..Default::default()
        };
        let dimensions = TerminalDimensions { columns, rows };
        let term = Arc::new(FairMutex::new(Term::new(
            config,
            &dimensions,
            proxy.clone(),
        )));

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let mut options = tty::Options {
            shell: Some(Shell::new(shell, vec!["-l".into()])),
            working_directory: Some(working_directory.to_path_buf()),
            drain_on_exit: false,
            ..Default::default()
        };
        options.env.insert("TERM".into(), "xterm-256color".into());
        options.env.insert("COLORTERM".into(), "truecolor".into());
        if let Some(path) = crate::command_env::executable_search_path() {
            options
                .env
                .insert("PATH".into(), path.to_string_lossy().into_owned());
        }

        let pty = tty::new(&options, window_size, 0)
            .with_context(|| format!("spawn terminal in {}", working_directory.display()))?;
        let event_loop = EventLoop::new(term.clone(), proxy, pty, false, false)
            .context("create Alacritty PTY event loop")?;
        let sender = event_loop.channel();
        sender_slot
            .set(sender.clone())
            .map_err(|_| anyhow::anyhow!("initialize Alacritty PTY sender"))?;
        event_loop.spawn();

        Ok(Self {
            term,
            sender,
            dirty,
            ui_events,
            window_size: shared_window_size,
            grid_size: (columns, rows),
        })
    }

    fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let bytes = bytes.into();
        if !bytes.is_empty() {
            let _ = self.sender.send(Msg::Input(bytes));
        }
    }

    fn resize(&mut self, columns: usize, rows: usize) {
        let columns = columns.max(TERMINAL_MIN_COLUMNS);
        let rows = rows.max(TERMINAL_MIN_ROWS);
        if self.grid_size == (columns, rows) {
            return;
        }

        self.grid_size = (columns, rows);
        let dimensions = TerminalDimensions { columns, rows };
        self.term.lock().resize(dimensions);
        let size = WindowSize {
            num_lines: rows.min(u16::MAX as usize) as u16,
            num_cols: columns.min(u16::MAX as usize) as u16,
            cell_width: TERMINAL_CELL_WIDTH.round() as u16,
            cell_height: TERMINAL_CELL_HEIGHT.round() as u16,
        };
        *self.window_size.lock() = size;
        let _ = self.sender.send(Msg::Resize(size));
        self.dirty.store(true, Ordering::Release);
    }

    fn mode(&self) -> TermMode {
        *self.term.lock().mode()
    }

    fn scroll(&self, lines: i32) {
        if lines == 0 {
            return;
        }
        self.term.lock().scroll_display(Scroll::Delta(lines));
        self.dirty.store(true, Ordering::Release);
    }

    fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    fn snapshot(
        &self,
        theme: Theme,
        selection_color: Hsla,
        render_cursor: bool,
    ) -> TerminalSnapshot {
        let term = self.term.lock();
        let content = term.renderable_content();
        let columns = self.grid_size.0;
        let rows = self.grid_size.1;
        let selection = content.selection;
        let cursor_row = content.cursor.point.line.0 + content.display_offset as i32;
        let cursor_column = content.cursor.point.column.0;
        let cursor_visible = render_cursor
            && !matches!(
                content.cursor.shape,
                alacritty_terminal::vte::ansi::CursorShape::Hidden
            );
        let mut cells = vec![TerminalCell::default(); columns * rows];

        for indexed in content.display_iter {
            let row = indexed.point.line.0 + content.display_offset as i32;
            let column = indexed.point.column.0;
            if row < 0 || row as usize >= rows || column >= columns {
                continue;
            }
            let cell = indexed.cell;
            let mut text = if cell.flags.contains(Flags::WIDE_CHAR_SPACER)
                || cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
                || cell.flags.contains(Flags::HIDDEN)
            {
                " ".to_owned()
            } else {
                cell.c.to_string()
            };
            if let Some(zerowidth) = cell.zerowidth() {
                text.extend(zerowidth);
            }

            let mut foreground = resolve_color(cell.fg, content.colors, theme, true);
            let mut background = resolve_color(cell.bg, content.colors, theme, false);
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut foreground, &mut background);
            }
            if cell.flags.intersects(Flags::DIM | Flags::DIM_BOLD) {
                foreground.l *= 0.7;
            }
            if selection.is_some_and(|selection| selection.contains(indexed.point)) {
                background = selection_color;
                foreground = theme.text;
            }
            if cursor_visible && row == cursor_row && column == cursor_column {
                background = theme.text;
                foreground = theme.inset;
            }
            let symbols_font = text.chars().any(uses_symbols_nerd_font);

            cells[row as usize * columns + column] = TerminalCell {
                text,
                foreground,
                background,
                bold: cell.flags.intersects(Flags::BOLD | Flags::BOLD_ITALIC),
                italic: cell.flags.intersects(Flags::ITALIC | Flags::BOLD_ITALIC),
                underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
                strikeout: cell.flags.contains(Flags::STRIKEOUT),
                symbols_font,
            };
        }

        let mut rendered_rows = Vec::with_capacity(rows);
        for row in cells.chunks(columns) {
            let mut text = String::new();
            let mut runs: Vec<TerminalRun> = Vec::new();
            for cell in row {
                let len = cell.text.len();
                text.push_str(&cell.text);
                let style = TerminalRunStyle {
                    foreground: cell.foreground,
                    background: cell.background,
                    bold: cell.bold,
                    italic: cell.italic,
                    underline: cell.underline,
                    strikeout: cell.strikeout,
                    symbols_font: cell.symbols_font,
                };
                if let Some(run) = runs.last_mut().filter(|run| run.style == style) {
                    run.len += len;
                } else {
                    runs.push(TerminalRun { len, style });
                }
            }
            rendered_rows.push(TerminalRow { text, runs });
        }

        TerminalSnapshot {
            rows: rendered_rows,
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.sender.send(Msg::Shutdown);
    }
}

#[derive(Clone)]
struct TerminalCell {
    text: String,
    foreground: Hsla,
    background: Hsla,
    bold: bool,
    italic: bool,
    underline: bool,
    strikeout: bool,
    symbols_font: bool,
}

impl Default for TerminalCell {
    fn default() -> Self {
        Self {
            text: " ".into(),
            foreground: rgb(0xe5e5e5).into(),
            background: rgb(0x151515).into(),
            bold: false,
            italic: false,
            underline: false,
            strikeout: false,
            symbols_font: false,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
struct TerminalRunStyle {
    foreground: Hsla,
    background: Hsla,
    bold: bool,
    italic: bool,
    underline: bool,
    strikeout: bool,
    symbols_font: bool,
}

struct TerminalRun {
    len: usize,
    style: TerminalRunStyle,
}

struct TerminalRow {
    text: String,
    runs: Vec<TerminalRun>,
}

struct TerminalSnapshot {
    rows: Vec<TerminalRow>,
}

struct TerminalCursorBlink {
    visible: bool,
    enabled: bool,
    epoch: usize,
    _task: Task<()>,
}

impl TerminalCursorBlink {
    fn new() -> Self {
        Self {
            visible: true,
            enabled: false,
            epoch: 0,
            _task: Task::ready(()),
        }
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        if self.enabled {
            return;
        }

        self.enabled = true;
        self.visible = true;
        let epoch = self.next_epoch();
        self.schedule_blink(epoch, TERMINAL_CURSOR_BLINK_INTERVAL, cx);
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if !self.enabled && self.visible {
            return;
        }

        self.enabled = false;
        self.visible = true;
        self.next_epoch();
        cx.notify();
    }

    fn pause(&mut self, cx: &mut Context<Self>) {
        if !self.enabled {
            return;
        }

        self.visible = true;
        let epoch = self.next_epoch();
        self.schedule_blink(epoch, TERMINAL_CURSOR_BLINK_PAUSE, cx);
        cx.notify();
    }

    fn blink(&mut self, epoch: usize, cx: &mut Context<Self>) {
        if !self.enabled || epoch != self.epoch {
            return;
        }

        self.visible = !self.visible;
        let epoch = self.next_epoch();
        self.schedule_blink(epoch, TERMINAL_CURSOR_BLINK_INTERVAL, cx);
        cx.notify();
    }

    fn schedule_blink(&mut self, epoch: usize, delay: Duration, cx: &mut Context<Self>) {
        self._task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| this.blink(epoch, cx));
            }
        });
    }

    fn next_epoch(&mut self) -> usize {
        self.epoch = self.epoch.wrapping_add(1);
        self.epoch
    }

    fn visible(&self) -> bool {
        self.visible
    }
}

pub struct TerminalView {
    session: Option<TerminalSession>,
    error: Option<String>,
    focus_handle: FocusHandle,
    working_directory: PathBuf,
    title: String,
    exited: bool,
    scroll_accumulator: f32,
    panel_width: f32,
    grid_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    selecting: bool,
    cursor_blink: gpui::Entity<TerminalCursorBlink>,
    cursor_focus_tracking_started: bool,
    _subscriptions: Vec<Subscription>,
}

impl TerminalView {
    pub fn new(working_directory: PathBuf, cx: &mut Context<Self>) -> Self {
        let session = TerminalSession::new(&working_directory, 52, 36);
        let (session, error) = match session {
            Ok(session) => (Some(session), None),
            Err(error) => (None, Some(error.to_string())),
        };

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(24))
                    .await;
                if this
                    .update(cx, |this, cx| {
                        if this.poll(cx) {
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

        let cursor_blink = cx.new(|_| TerminalCursorBlink::new());
        let subscriptions = vec![cx.observe(&cursor_blink, |_, _, cx| cx.notify())];

        Self {
            session,
            error,
            focus_handle: cx.focus_handle(),
            title: "Terminal".into(),
            working_directory,
            exited: false,
            scroll_accumulator: 0.0,
            panel_width: DEFAULT_RIGHT_PANEL_WIDTH,
            grid_bounds: Rc::new(Cell::new(None)),
            selecting: false,
            cursor_blink,
            cursor_focus_tracking_started: false,
            _subscriptions: subscriptions,
        }
    }

    pub fn working_directory(&self) -> &Path {
        &self.working_directory
    }

    pub fn set_panel_width(&mut self, width: f32) {
        self.panel_width = width;
    }

    fn poll(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        let mut changed = session.take_dirty();
        while let Ok(event) = session.ui_events.try_recv() {
            changed = true;
            match event {
                TerminalUiEvent::Title(title) => self.title = title,
                TerminalUiEvent::ResetTitle => self.title = "Terminal".into(),
                TerminalUiEvent::ClipboardStore(text) => {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                }
                TerminalUiEvent::ClipboardLoad(formatter) => {
                    let text = cx
                        .read_from_clipboard()
                        .and_then(|item| item.text())
                        .unwrap_or_default();
                    session.write(formatter(&text).into_bytes());
                }
                TerminalUiEvent::Exited => self.exited = true,
            }
        }
        changed
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.pause_cursor_blink(cx);
        let keystroke = &event.keystroke;
        if keystroke.modifiers.platform && keystroke.key.eq_ignore_ascii_case("c") {
            self.copy_selection(cx);
            window.prevent_default();
            cx.stop_propagation();
            return;
        }
        if keystroke.modifiers.platform && keystroke.key.eq_ignore_ascii_case("a") {
            self.select_all(cx);
            window.prevent_default();
            cx.stop_propagation();
            return;
        }

        let Some(session) = &self.session else {
            return;
        };
        if keystroke.modifiers.platform && keystroke.key.eq_ignore_ascii_case("v") {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                session.term.lock().selection = None;
                let bytes = bracketed_paste(text, session.mode());
                session.write(bytes);
                session.dirty.store(true, Ordering::Release);
                window.prevent_default();
                cx.stop_propagation();
            }
            return;
        }

        if let Some(bytes) = terminal_key_bytes(keystroke, session.mode()) {
            session.term.lock().selection = None;
            session.write(bytes);
            session.dirty.store(true, Ordering::Release);
            window.prevent_default();
            cx.stop_propagation();
        }
    }

    fn grid_point_for_position(
        &self,
        position: Point<Pixels>,
        clamp_to_grid: bool,
    ) -> Option<(TerminalPoint, Side)> {
        let bounds = self.grid_bounds.get()?;
        let session = self.session.as_ref()?;
        let display_offset = session.term.lock().grid().display_offset();
        terminal_grid_point(
            bounds,
            position,
            session.grid_size.0,
            session.grid_size.1,
            display_offset,
            clamp_to_grid,
        )
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        let Some((point, side)) = self.grid_point_for_position(event.position, false) else {
            return;
        };
        let Some(session) = &self.session else {
            return;
        };

        let mut term = session.term.lock();
        if event.modifiers.shift
            && let Some(selection) = term.selection.as_mut()
        {
            selection.update(point, side);
        } else {
            let selection_type = match event.click_count {
                2 => SelectionType::Semantic,
                count if count >= 3 => SelectionType::Lines,
                _ => SelectionType::Simple,
            };
            term.selection = Some(Selection::new(selection_type, point, side));
        }
        drop(term);

        self.selecting = true;
        session.dirty.store(true, Ordering::Release);
        cx.stop_propagation();
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let Some((point, side)) = self.grid_point_for_position(event.position, true) else {
            return;
        };
        let Some(session) = &self.session else {
            return;
        };

        if let Some(selection) = session.term.lock().selection.as_mut() {
            selection.update(point, side);
            session.dirty.store(true, Ordering::Release);
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    fn selected_text(&self) -> Option<String> {
        self.session
            .as_ref()?
            .term
            .lock()
            .selection_to_string()
            .filter(|text| !text.is_empty())
    }

    fn copy_selection(&self, cx: &mut App) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, text: String, cx: &mut Context<Self>) {
        self.pause_cursor_blink(cx);
        let Some(session) = &self.session else {
            return;
        };
        session.term.lock().selection = None;
        session.write(bracketed_paste(text, session.mode()));
        session.dirty.store(true, Ordering::Release);
        cx.notify();
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let mut term = session.term.lock();
        let start = TerminalPoint::new(term.topmost_line(), Column(0));
        let end = TerminalPoint::new(term.bottommost_line(), term.last_column());
        let mut selection = Selection::new(SelectionType::Simple, start, Side::Left);
        selection.update(end, Side::Right);
        term.selection = Some(selection);
        drop(term);
        session.dirty.store(true, Ordering::Release);
        cx.notify();
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = &self.session else {
            return;
        };
        let delta = match event.delta {
            ScrollDelta::Pixels(delta) => f32::from(delta.y) / TERMINAL_CELL_HEIGHT,
            ScrollDelta::Lines(delta) => delta.y,
        };
        self.scroll_accumulator += delta;
        let lines = self.scroll_accumulator.trunc() as i32;
        if lines != 0 {
            self.scroll_accumulator -= lines as f32;
            session.scroll(lines);
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn ensure_cursor_focus_tracking(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cursor_focus_tracking_started {
            return;
        }
        self.cursor_focus_tracking_started = true;

        let focus_handle = self.focus_handle.clone();
        self._subscriptions.extend([
            cx.observe_window_activation(window, |terminal, window, cx| {
                terminal.update_cursor_blinking(window, cx);
            }),
            cx.on_focus(&focus_handle, window, |terminal, window, cx| {
                terminal.update_cursor_blinking(window, cx);
            }),
            cx.on_blur(&focus_handle, window, |terminal, window, cx| {
                terminal.update_cursor_blinking(window, cx);
            }),
        ]);
        self.update_cursor_blinking(window, cx);
    }

    fn update_cursor_blinking(&mut self, window: &Window, cx: &mut Context<Self>) {
        let focused = window.is_window_active() && self.focus_handle.is_focused(window);
        self.cursor_blink.update(cx, |cursor, cx| {
            if focused {
                cursor.start(cx);
            } else {
                cursor.stop(cx);
            }
        });
    }

    fn pause_cursor_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_blink.update(cx, |cursor, cx| cursor.pause(cx));
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_cursor_focus_tracking(window, cx);
        let theme = Theme::current(cx);
        let selection_color = cx.theme().selection;
        let viewport = window.viewport_size();
        let panel_width = self.panel_width;
        let body_height = (f32::from(viewport.height) - 48.0 - TERMINAL_TOOLBAR_HEIGHT).max(120.0);
        let columns = ((panel_width - TERMINAL_PADDING_X * 2.0) / TERMINAL_CELL_WIDTH)
            .floor()
            .max(TERMINAL_MIN_COLUMNS as f32) as usize;
        let rows = ((body_height - TERMINAL_PADDING_Y * 2.0) / TERMINAL_CELL_HEIGHT)
            .floor()
            .max(TERMINAL_MIN_ROWS as f32) as usize;

        let terminal_focused = window.is_window_active() && self.focus_handle.is_focused(window);
        let render_cursor = terminal_cursor_should_be_visible(
            terminal_focused,
            self.cursor_blink.read(cx).visible(),
        );
        let snapshot = self.session.as_mut().map(|session| {
            session.resize(columns, rows);
            session.snapshot(theme, selection_color, render_cursor)
        });
        let title = if self.title.trim().is_empty() {
            "Terminal"
        } else {
            &self.title
        };
        let directory = self
            .working_directory
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("workspace")
            .to_owned();

        let mut screen = div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .relative()
            .cursor_text()
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move));

        if let Some(snapshot) = snapshot {
            for row in snapshot.rows {
                let runs = row
                    .runs
                    .into_iter()
                    .map(|run| {
                        let mut run_font = font(if run.style.symbols_font {
                            "Symbols Nerd Font Mono"
                        } else {
                            "JetBrains Mono"
                        });
                        if run.style.bold {
                            run_font.weight = FontWeight::BOLD;
                        }
                        if run.style.italic {
                            run_font.style = FontStyle::Italic;
                        }
                        TextRun {
                            len: run.len,
                            font: run_font,
                            color: run.style.foreground,
                            background_color: Some(run.style.background),
                            underline: run.style.underline.then_some(UnderlineStyle {
                                thickness: px(1.0),
                                color: Some(run.style.foreground),
                                wavy: false,
                            }),
                            strikethrough: run.style.strikeout.then_some(StrikethroughStyle {
                                thickness: px(1.0),
                                color: Some(run.style.foreground),
                            }),
                        }
                    })
                    .collect::<Vec<_>>();
                screen = screen.child(
                    div()
                        .h(px(TERMINAL_CELL_HEIGHT))
                        .flex_none()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_size(px(TERMINAL_FONT_SIZE))
                        .line_height(px(TERMINAL_CELL_HEIGHT))
                        .child(StyledText::new(row.text).with_runs(runs)),
                );
            }
        } else {
            screen = screen.child(
                div()
                    .p(px(12.0))
                    .text_size(px(11.0))
                    .line_height(px(17.0))
                    .text_color(theme.danger)
                    .child(
                        self.error
                            .clone()
                            .unwrap_or_else(|| "Unable to start terminal".into()),
                    ),
            );
        }

        let grid_bounds = self.grid_bounds.clone();
        screen = screen.child(
            canvas(
                move |bounds, _, _| grid_bounds.set(Some(bounds)),
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0(),
        );

        let context_terminal = cx.entity();
        let focus_handle = self.focus_handle.clone();
        let screen = screen.context_menu_with_id("terminal-context-menu", move |menu, _, cx| {
            let has_selection = context_terminal.read(cx).selected_text().is_some();
            let can_paste = cx
                .read_from_clipboard()
                .and_then(|item| item.text())
                .is_some_and(|text| !text.is_empty());
            let has_session = context_terminal.read(cx).session.is_some();

            let copy_terminal = context_terminal.clone();
            let paste_terminal = context_terminal.clone();
            let select_all_terminal = context_terminal.clone();
            menu.action_context(focus_handle.clone())
                .min_w(px(150.0))
                .item(
                    PopupMenuItem::new("Copy")
                        .disabled(!has_selection)
                        .on_click(move |_, _, cx| {
                            let selected_text = { copy_terminal.read(cx).selected_text() };
                            if let Some(text) = selected_text {
                                cx.write_to_clipboard(ClipboardItem::new_string(text));
                            }
                        }),
                )
                .item(
                    PopupMenuItem::new("Paste")
                        .disabled(!can_paste)
                        .on_click(move |_, _, cx| {
                            let Some(text) = cx.read_from_clipboard().and_then(|item| item.text())
                            else {
                                return;
                            };
                            paste_terminal.update(cx, |terminal, cx| terminal.paste(text, cx));
                        }),
                )
                .separator()
                .item(
                    PopupMenuItem::new("Select All")
                        .disabled(!has_session)
                        .on_click(move |_, _, cx| {
                            select_all_terminal.update(cx, |terminal, cx| terminal.select_all(cx));
                        }),
                )
        });

        let grid = div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .px(px(TERMINAL_PADDING_X))
            .py(px(TERMINAL_PADDING_Y))
            .bg(theme.inset)
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(screen);

        div()
            .id("alacritty-terminal")
            .key_context("Terminal")
            .track_focus(&self.focus_handle)
            .size_full()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .bg(theme.inset)
            .child(
                div()
                    .h(px(TERMINAL_TOOLBAR_HEIGHT))
                    .flex_none()
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .bg(theme.surface)
                    .child(
                        div()
                            .w(px(6.0))
                            .h(px(6.0))
                            .rounded_full()
                            .bg(if self.exited {
                                theme.danger
                            } else {
                                theme.accent
                            }),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(px(10.5))
                            .text_color(theme.text_secondary)
                            .child(SharedString::from(title.to_owned())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(10.0))
                            .text_color(theme.text_tertiary)
                            .child(directory),
                    ),
            )
            .child(grid)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
    }
}

fn terminal_cursor_should_be_visible(focused: bool, blink_visible: bool) -> bool {
    !focused || blink_visible
}

fn terminal_grid_point(
    bounds: Bounds<Pixels>,
    position: Point<Pixels>,
    columns: usize,
    rows: usize,
    display_offset: usize,
    clamp_to_grid: bool,
) -> Option<(TerminalPoint, Side)> {
    if columns == 0 || rows == 0 || (!clamp_to_grid && !bounds.contains(&position)) {
        return None;
    }

    let x = f32::from(position.x - bounds.origin.x);
    let y = f32::from(position.y - bounds.origin.y);
    let max_x = columns as f32 * TERMINAL_CELL_WIDTH;
    let max_y = rows as f32 * TERMINAL_CELL_HEIGHT;
    let x = x.clamp(0.0, max_x);
    let y = y.clamp(0.0, max_y);
    let column = ((x / TERMINAL_CELL_WIDTH).floor() as usize).min(columns - 1);
    let viewport_row = ((y / TERMINAL_CELL_HEIGHT).floor() as usize).min(rows - 1) as i32;
    let side = if x >= max_x || x % TERMINAL_CELL_WIDTH >= TERMINAL_CELL_WIDTH / 2.0 {
        Side::Right
    } else {
        Side::Left
    };
    let line = Line(viewport_row - display_offset.min(i32::MAX as usize) as i32);

    Some((TerminalPoint::new(line, Column(column)), side))
}

fn bracketed_paste(text: String, mode: TermMode) -> Vec<u8> {
    if mode.contains(TermMode::BRACKETED_PASTE) {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    }
}

fn terminal_key_bytes(keystroke: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    let modifiers = keystroke.modifiers;
    let key = keystroke.key.as_str();

    if modifiers.platform {
        return match key {
            "left" => Some(vec![0x01]),
            "right" => Some(vec![0x05]),
            "backspace" => Some(vec![0x15]),
            _ => None,
        };
    }

    let modifier = 1
        + u8::from(modifiers.shift)
        + u8::from(modifiers.alt) * 2
        + u8::from(modifiers.control) * 4;
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let special = match key {
        "enter" | "return" => Some("\r".to_owned()),
        "tab" if modifiers.shift => Some("\x1b[Z".to_owned()),
        "tab" => Some("\t".to_owned()),
        "backspace" => Some("\x7f".to_owned()),
        "escape" => Some("\x1b".to_owned()),
        "up" => Some(cursor_sequence('A', modifier, app_cursor)),
        "down" => Some(cursor_sequence('B', modifier, app_cursor)),
        "right" => Some(cursor_sequence('C', modifier, app_cursor)),
        "left" => Some(cursor_sequence('D', modifier, app_cursor)),
        "home" => Some(csi_sequence('H', modifier)),
        "end" => Some(csi_sequence('F', modifier)),
        "insert" => Some(tilde_sequence(2, modifier)),
        "delete" | "forwarddelete" => Some(tilde_sequence(3, modifier)),
        "pageup" => Some(tilde_sequence(5, modifier)),
        "pagedown" => Some(tilde_sequence(6, modifier)),
        "f1" => Some(function_sequence('P', modifier)),
        "f2" => Some(function_sequence('Q', modifier)),
        "f3" => Some(function_sequence('R', modifier)),
        "f4" => Some(function_sequence('S', modifier)),
        "f5" => Some(tilde_sequence(15, modifier)),
        "f6" => Some(tilde_sequence(17, modifier)),
        "f7" => Some(tilde_sequence(18, modifier)),
        "f8" => Some(tilde_sequence(19, modifier)),
        "f9" => Some(tilde_sequence(20, modifier)),
        "f10" => Some(tilde_sequence(21, modifier)),
        "f11" => Some(tilde_sequence(23, modifier)),
        "f12" => Some(tilde_sequence(24, modifier)),
        _ => None,
    };
    if let Some(special) = special {
        return Some(special.into_bytes());
    }

    let text = keystroke
        .key_char
        .as_deref()
        .or_else(|| (key.chars().count() == 1).then_some(key))?;
    let mut bytes = if modifiers.control {
        control_bytes(text)?
    } else {
        text.as_bytes().to_vec()
    };
    if modifiers.alt {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

fn control_bytes(text: &str) -> Option<Vec<u8>> {
    let character = text.chars().next()?.to_ascii_lowercase();
    let byte = match character {
        ' ' | '@' => 0,
        'a'..='z' => character as u8 - b'a' + 1,
        '[' => 27,
        '\\' => 28,
        ']' => 29,
        '^' => 30,
        '_' => 31,
        '?' => 127,
        _ => return None,
    };
    Some(vec![byte])
}

fn cursor_sequence(final_byte: char, modifier: u8, app_cursor: bool) -> String {
    if modifier == 1 {
        format!("\x1b{}{}", if app_cursor { 'O' } else { '[' }, final_byte)
    } else {
        format!("\x1b[1;{modifier}{final_byte}")
    }
}

fn csi_sequence(final_byte: char, modifier: u8) -> String {
    if modifier == 1 {
        format!("\x1b[{final_byte}")
    } else {
        format!("\x1b[1;{modifier}{final_byte}")
    }
}

fn function_sequence(final_byte: char, modifier: u8) -> String {
    if modifier == 1 {
        format!("\x1bO{final_byte}")
    } else {
        format!("\x1b[1;{modifier}{final_byte}")
    }
}

fn tilde_sequence(number: u8, modifier: u8) -> String {
    if modifier == 1 {
        format!("\x1b[{number}~")
    } else {
        format!("\x1b[{number};{modifier}~")
    }
}

fn uses_symbols_nerd_font(character: char) -> bool {
    let codepoint = character as u32;

    // JetBrains Mono includes Powerline's branch and separator glyphs. Keep those
    // in the primary face so they retain terminal-cell metrics; use the bundled
    // Symbols Nerd Font for the remaining private-use icon ranges.
    !matches!(codepoint, 0xe0a0..=0xe0d7)
        && matches!(
            codepoint,
            0xe000..=0xf8ff | 0xf0000..=0xffffd | 0x100000..=0x10fffd
        )
}

fn resolve_color(
    color: Color,
    colors: &alacritty_terminal::term::color::Colors,
    theme: Theme,
    foreground: bool,
) -> Hsla {
    let rgb = match color {
        Color::Spec(color) => Some(color),
        Color::Indexed(index) => {
            colors[index as usize].or_else(|| Some(terminal_rgb(index as usize)))
        }
        Color::Named(NamedColor::Foreground | NamedColor::BrightForeground) => None,
        Color::Named(NamedColor::Background) => return theme.inset,
        Color::Named(NamedColor::Cursor) => return theme.text,
        Color::Named(named) => {
            colors[named as usize].or_else(|| Some(terminal_rgb(named as usize)))
        }
    };
    rgb.map(rgb_to_hsla)
        .unwrap_or(if foreground { theme.text } else { theme.inset })
}

fn rgb_to_hsla(color: Rgb) -> Hsla {
    rgb((u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)).into()
}

fn terminal_rgb(index: usize) -> Rgb {
    const ANSI: [u32; 16] = [
        0x1d1f21, 0xcc6666, 0xb5bd68, 0xf0c674, 0x81a2be, 0xb294bb, 0x8abeb7, 0xc5c8c6, 0x666666,
        0xd54e53, 0xb9ca4a, 0xe7c547, 0x7aa6da, 0xc397d8, 0x70c0b1, 0xeaeaea,
    ];
    let value = match index {
        0..=15 => ANSI[index],
        16..=231 => {
            let index = index - 16;
            let channel = |value: usize| {
                if value == 0 {
                    0
                } else {
                    55 + value as u32 * 40
                }
            };
            let red = channel(index / 36);
            let green = channel((index / 6) % 6);
            let blue = channel(index % 6);
            (red << 16) | (green << 8) | blue
        }
        232..=255 => {
            let value = 8 + (index as u32 - 232) * 10;
            (value << 16) | (value << 8) | value
        }
        value
            if value >= NamedColor::DimBlack as usize && value <= NamedColor::DimWhite as usize =>
        {
            let base = ANSI[value - NamedColor::DimBlack as usize];
            let dim = |channel: u32| channel * 2 / 3;
            (dim((base >> 16) & 0xff) << 16) | (dim((base >> 8) & 0xff) << 8) | dim(base & 0xff)
        }
        _ => 0xe5e5e5,
    };
    Rgb {
        r: (value >> 16) as u8,
        g: (value >> 8) as u8,
        b: value as u8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Modifiers, point, size};

    fn key(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            key: key.into(),
            key_char: key_char.map(str::to_owned),
            modifiers,
        }
    }

    #[test]
    fn encodes_terminal_control_and_cursor_keys() {
        assert_eq!(
            terminal_key_bytes(
                &key(
                    "c",
                    Some("c"),
                    Modifiers {
                        control: true,
                        ..Default::default()
                    }
                ),
                TermMode::empty()
            ),
            Some(vec![3])
        );
        assert_eq!(
            terminal_key_bytes(&key("up", None, Modifiers::default()), TermMode::APP_CURSOR),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            terminal_key_bytes(
                &key(
                    "left",
                    None,
                    Modifiers {
                        control: true,
                        ..Default::default()
                    }
                ),
                TermMode::empty()
            ),
            Some(b"\x1b[1;5D".to_vec())
        );
    }

    #[test]
    fn wraps_bracketed_paste_only_when_requested() {
        assert_eq!(
            bracketed_paste("hello".into(), TermMode::BRACKETED_PASTE),
            b"\x1b[200~hello\x1b[201~"
        );
        assert_eq!(bracketed_paste("hello".into(), TermMode::empty()), b"hello");
    }

    #[test]
    fn cursor_blinks_only_while_terminal_is_focused() {
        assert!(terminal_cursor_should_be_visible(false, false));
        assert!(terminal_cursor_should_be_visible(false, true));
        assert!(!terminal_cursor_should_be_visible(true, false));
        assert!(terminal_cursor_should_be_visible(true, true));
    }

    #[test]
    fn maps_pointer_positions_into_scrollback_grid_coordinates() {
        let bounds = Bounds::new(point(px(10.0), px(20.0)), size(px(72.0), px(64.0)));
        let position = point(
            px(10.0 + TERMINAL_CELL_WIDTH * 2.0 + 5.0),
            px(20.0 + TERMINAL_CELL_HEIGHT + 8.0),
        );

        assert_eq!(
            terminal_grid_point(bounds, position, 10, 4, 3, false),
            Some((TerminalPoint::new(Line(-2), Column(2)), Side::Right))
        );
        assert_eq!(
            terminal_grid_point(bounds, point(px(0.0), px(0.0)), 10, 4, 3, false),
            None
        );
    }

    #[test]
    fn clamps_selection_drags_to_the_terminal_grid_edges() {
        let bounds = Bounds::new(point(px(10.0), px(20.0)), size(px(72.0), px(64.0)));

        assert_eq!(
            terminal_grid_point(bounds, point(px(500.0), px(500.0)), 10, 4, 0, true),
            Some((TerminalPoint::new(Line(3), Column(9)), Side::Right))
        );
        assert_eq!(
            terminal_grid_point(bounds, point(px(-50.0), px(-50.0)), 10, 4, 3, true),
            Some((TerminalPoint::new(Line(-3), Column(0)), Side::Left))
        );
    }

    #[test]
    fn keeps_powerline_glyphs_in_jetbrains_mono() {
        assert!(!uses_symbols_nerd_font('\u{e0a0}'));
        assert!(!uses_symbols_nerd_font('\u{e0b0}'));
        assert!(uses_symbols_nerd_font('\u{f121}'));
    }
}
