use std::collections::VecDeque;
use std::time::Instant;

/// OSC 133 command detection state
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandState {
    Idle,
    CommandStart,
    Executing,
}

/// Result of a detected command execution
#[derive(Clone, Debug, serde::Serialize)]
pub struct CommandResult {
    /// Shell status, or -1 when completion was inferred or timed out.
    pub exit_code: i32,
    /// Elapsed time since tracking began in this PTY.
    pub duration_ms: u64,
    /// Completion source exposed to MCP and agent clients.
    pub method: String,
    /// Parsed terminal text belonging to this command, excluding control sequences.
    pub stdout: String,
}

/// Tracks a pending command for collecting output
pub(crate) struct PendingCommand {
    /// Monotonic start retained across shell execution markers.
    pub start_time: Instant,
    /// UTF-8 text collected by the VTE performer.
    pub output_buf: Vec<u8>,
    /// UTF-8 byte offset used for CR/backspace overprinting within the current line.
    pub output_cursor: usize,
    /// Registered before input injection; retained after caller timeout.
    pub waiter: Option<tokio::sync::oneshot::Sender<CommandResult>>,
}

/// Atomic parser completion, delivered once by the session extraction path.
pub(crate) struct CompletedCommand {
    /// Public result also forwarded to the event bus.
    pub result: CommandResult,
    /// Original caller, never reassigned to a subsequent execution.
    pub waiter: Option<tokio::sync::oneshot::Sender<CommandResult>>,
}

impl PendingCommand {
    /// Apply printable text to the captured line, retaining at most 1 MiB of UTF-8.
    pub fn capture_char(&mut self, c: char) {
        const OUTPUT_LIMIT: usize = 1024 * 1024;
        let mut bytes = [0; 4];
        let encoded = c.encode_utf8(&mut bytes).as_bytes();
        if c == '\n' {
            self.output_buf.push(b'\n');
            self.output_cursor = self.output_buf.len();
        } else {
            let mut end = self.output_cursor;
            if end < self.output_buf.len() {
                end += 1;
                while end < self.output_buf.len() && self.output_buf[end] & 0xc0 == 0x80 {
                    end += 1;
                }
            }
            self.output_buf.splice(self.output_cursor..end, encoded.iter().copied());
            self.output_cursor += encoded.len();
        }
        if self.output_buf.len() > OUTPUT_LIMIT {
            let mut end = OUTPUT_LIMIT / 2;
            while self.output_buf[end] & 0xc0 == 0x80 {
                end += 1;
            }
            self.output_buf.drain(..end);
            self.output_cursor = self.output_cursor.saturating_sub(end);
        }
    }

    /// Model overprinting without exposing erased prompt padding (notably Zsh `PROMPT_SP`).
    pub fn carriage_return(&mut self) {
        self.output_cursor =
            self.output_buf.iter().rposition(|b| *b == b'\n').map_or(0, |index| index + 1);
        if self.output_buf[self.output_cursor..].iter().all(|b| *b == b' ') {
            self.output_buf.truncate(self.output_cursor);
        }
    }

    /// Keep backspace inside the current line and on a UTF-8 character boundary.
    pub fn backspace(&mut self) {
        if self.output_cursor == 0 || self.output_buf[self.output_cursor - 1] == b'\n' {
            return;
        }
        self.output_cursor -= 1;
        while self.output_cursor > 0 && self.output_buf[self.output_cursor] & 0xc0 == 0x80 {
            self.output_cursor -= 1;
        }
    }

    /// Transfer captured text and the registered waiter together on completion.
    pub fn complete(self, exit_code: i32, method: &str) -> CompletedCommand {
        CompletedCommand {
            result: CommandResult {
                exit_code,
                duration_ms: self.start_time.elapsed().as_millis() as u64,
                method: method.to_string(),
                stdout: String::from_utf8_lossy(&self.output_buf).into_owned(),
            },
            waiter: self.waiter,
        }
    }
}

/// DEC mode 2026 synchronized output events
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncEvent {
    Start,
    Stop,
}

/// OSC 9 / OSC 777 / BEL notification detection result
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OscAction {
    Bell,
    Notify { title: Option<String>, body: String },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum MouseProtocol {
    #[default]
    None,
    X10,
    Normal,
    Button,
    Any,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum MouseEncoding {
    #[default]
    Default,
    Sgr,
    SgrPixels,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PrivateModes {
    pub mouse: MouseProtocol,
    pub encoding: MouseEncoding,
    pub cursor_keys: bool,
    pub keypad: bool,
    pub bracketed_paste: bool,
    pub focus_event: bool,
}

impl PrivateModes {
    pub(crate) fn soft_reset(&mut self) {
        self.cursor_keys = false;
        self.keypad = false;
        self.bracketed_paste = false;
        self.focus_event = false;
    }

    pub(crate) fn write_replay(self, out: &mut String) {
        // Install the encoding first so a mouse event racing replay cannot be
        // emitted using the wrong wire format.
        match self.encoding {
            MouseEncoding::Default => {}
            MouseEncoding::Sgr => out.push_str("\x1b[?1006h"),
            MouseEncoding::SgrPixels => out.push_str("\x1b[?1016h"),
        }
        match self.mouse {
            MouseProtocol::None => {}
            MouseProtocol::X10 => out.push_str("\x1b[?9h"),
            MouseProtocol::Normal => out.push_str("\x1b[?1000h"),
            MouseProtocol::Button => out.push_str("\x1b[?1002h"),
            MouseProtocol::Any => out.push_str("\x1b[?1003h"),
        }
        if self.cursor_keys {
            out.push_str("\x1b[?1h");
        }
        if self.keypad {
            out.push_str("\x1b[?66h");
        }
        if self.bracketed_paste {
            out.push_str("\x1b[?2004h");
        }
        // Focus events (1004) are tracked but intentionally not replayed: a
        // reconnect can otherwise trigger a focus-report feedback storm.
    }
}

#[derive(Clone, Copy, Default)]
pub struct CellAttrs {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub strikethrough: bool,
}

#[derive(Clone, Copy)]
pub enum Color {
    Indexed(u8),
    Rgb(u8, u8, u8),
}

pub(crate) const MAX_COMBINING: usize = 3;

#[derive(Clone, Copy)]
pub(crate) struct Cell {
    pub ch: char,
    pub combining: [char; MAX_COMBINING],
    pub combining_len: u8,
    pub attrs: CellAttrs,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            combining: ['\0'; MAX_COMBINING],
            combining_len: 0,
            attrs: CellAttrs::default(),
        }
    }
}

impl Cell {
    pub(crate) fn push_combining(&mut self, c: char) {
        let len = self.combining_len as usize;
        if len < MAX_COMBINING {
            self.combining[len] = c;
            self.combining_len += 1;
        }
    }

    pub(crate) fn write_to(&self, out: &mut String) {
        out.push(self.ch);
        for i in 0..self.combining_len as usize {
            out.push(self.combining[i]);
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct CursorState {
    pub row: usize,
    pub col: usize,
    pub attrs: CellAttrs,
}

#[derive(Clone)]
pub(crate) struct ScreenBuffer {
    pub cells: Vec<Vec<Cell>>,
    pub cursor: CursorState,
    pub scroll_top: usize,
    pub scroll_bottom: usize,
    pub cols: usize,
    pub rows: usize,
}

impl ScreenBuffer {
    pub(crate) fn new(cols: usize, rows: usize) -> Self {
        Self {
            cells: vec![vec![Cell::default(); cols]; rows],
            cursor: CursorState::default(),
            scroll_top: 0,
            scroll_bottom: rows - 1,
            cols,
            rows,
        }
    }

    pub(crate) fn resize(
        &mut self,
        cols: usize,
        rows: usize,
        mut scrollback: Option<&mut VecDeque<Vec<Cell>>>,
    ) {
        let old_rows = self.cells.len();
        let old_cols = if old_rows > 0 { self.cells[0].len() } else { 0 };

        if rows < old_rows {
            let mut excess = old_rows - rows;
            // Trim blank rows below the cursor from the bottom first.
            while excess > 0
                && self.cells.len() > self.cursor.row + 1
                && self.cells.last().is_some_and(|last| {
                    last.iter().all(|c| {
                        (c.ch == ' ' || c.ch == '\0') && !super::render::has_attrs(&c.attrs)
                    })
                })
            {
                self.cells.pop();
                excess -= 1;
            }
            // Rows that still don't fit move from the top into scrollback
            // (primary screen only) instead of truncating the bottom, where
            // the most recent output and the prompt live.
            for _ in 0..excess {
                let row = self.cells.remove(0);
                if let Some(sb) = scrollback.as_deref_mut() {
                    sb.push_back(row);
                    if sb.len() > 10000 {
                        sb.pop_front();
                    }
                }
                self.cursor.row = self.cursor.row.saturating_sub(1);
            }
        } else if rows > old_rows {
            self.cells.resize(rows, vec![Cell::default(); cols]);
        }
        if cols != old_cols {
            for row in &mut self.cells {
                row.resize(cols, Cell::default());
            }
        }
        self.cols = cols;
        self.rows = rows;
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        if self.cursor.row >= rows {
            self.cursor.row = rows - 1;
        }
        if self.cursor.col >= cols {
            self.cursor.col = cols - 1;
        }
    }

    pub(crate) fn scroll_up(&mut self, scrollback: &mut VecDeque<Vec<Cell>>) {
        let row = self.cells.remove(self.scroll_top);
        if self.scroll_top == 0 {
            scrollback.push_back(row);
            if scrollback.len() > 10000 {
                scrollback.pop_front();
            }
        }
        self.cells.insert(self.scroll_bottom, vec![Cell::default(); self.cols]);
    }

    pub(crate) fn scroll_down(&mut self) {
        self.cells.remove(self.scroll_bottom);
        self.cells.insert(self.scroll_top, vec![Cell::default(); self.cols]);
    }
}
