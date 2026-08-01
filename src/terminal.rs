use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::tty::{self, Shell};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use anyhow::{Context as _, Result};
use crossbeam_channel::{Receiver, Sender, unbounded};
use gpui::{
    App, Context, FocusHandle, Focusable, FontStyle, FontWeight, Hsla, InteractiveElement,
    IntoElement, KeyDownEvent, Keystroke, MouseButton, ParentElement, Render, ScrollDelta,
    ScrollWheelEvent, SharedString, StrikethroughStyle, Styled, StyledText, TextRun, Timer,
    UnderlineStyle, Window, div, font, px, rgb,
};
use parking_lot::Mutex;

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

    fn snapshot(&self, theme: Theme) -> TerminalSnapshot {
        let term = self.term.lock();
        let content = term.renderable_content();
        let columns = self.grid_size.0;
        let rows = self.grid_size.1;
        let cursor_row = content.cursor.point.line.0 + content.display_offset as i32;
        let cursor_column = content.cursor.point.column.0;
        let cursor_visible = !matches!(
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

pub struct TerminalView {
    session: Option<TerminalSession>,
    error: Option<String>,
    focus_handle: FocusHandle,
    working_directory: PathBuf,
    title: String,
    exited: bool,
    scroll_accumulator: f32,
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
                Timer::after(Duration::from_millis(24)).await;
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

        Self {
            session,
            error,
            focus_handle: cx.focus_handle(),
            title: "Terminal".into(),
            working_directory,
            exited: false,
            scroll_accumulator: 0.0,
        }
    }

    pub fn working_directory(&self) -> &Path {
        &self.working_directory
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
        let Some(session) = &self.session else {
            return;
        };
        let keystroke = &event.keystroke;
        if keystroke.modifiers.platform && keystroke.key.eq_ignore_ascii_case("v") {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                let bytes = bracketed_paste(text, session.mode());
                session.write(bytes);
                window.prevent_default();
                cx.stop_propagation();
            }
            return;
        }

        if let Some(bytes) = terminal_key_bytes(keystroke, session.mode()) {
            session.write(bytes);
            window.prevent_default();
            cx.stop_propagation();
        }
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
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current(cx);
        let viewport = window.viewport_size();
        let panel_width = (f32::from(viewport.width) * 0.4).clamp(360.0, 460.0);
        let body_height = (f32::from(viewport.height) - 48.0 - TERMINAL_TOOLBAR_HEIGHT).max(120.0);
        let columns = ((panel_width - TERMINAL_PADDING_X * 2.0) / TERMINAL_CELL_WIDTH)
            .floor()
            .max(TERMINAL_MIN_COLUMNS as f32) as usize;
        let rows = ((body_height - TERMINAL_PADDING_Y * 2.0) / TERMINAL_CELL_HEIGHT)
            .floor()
            .max(TERMINAL_MIN_ROWS as f32) as usize;

        let snapshot = self.session.as_mut().map(|session| {
            session.resize(columns, rows);
            session.snapshot(theme)
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

        let mut grid = div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .px(px(TERMINAL_PADDING_X))
            .py(px(TERMINAL_PADDING_Y))
            .bg(theme.inset)
            .overflow_hidden()
            .flex()
            .flex_col();

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
                grid = grid.child(
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
            grid = grid.child(
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
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus_handle);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
    }
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
    use gpui::Modifiers;

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
    fn keeps_powerline_glyphs_in_jetbrains_mono() {
        assert!(!uses_symbols_nerd_font('\u{e0a0}'));
        assert!(!uses_symbols_nerd_font('\u{e0b0}'));
        assert!(uses_symbols_nerd_font('\u{f121}'));
    }
}
