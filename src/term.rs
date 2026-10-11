use std::collections::VecDeque;

use unicode_width::UnicodeWidthChar;

use crate::config::{self, CursorStyle};

fn default_fg() -> u32 {
    config::get().colors.foreground
}

fn default_bg() -> u32 {
    config::get().colors.background
}

fn palette(n: usize) -> u32 {
    config::get().colors.palette[n]
}

pub const BOLD: u8 = 1;
pub const UNDERLINE: u8 = 2;
pub const REVERSE: u8 = 4;
const WRAP: u8 = 8;
pub const DIM: u8 = 16;
pub const STRIKE: u8 = 32;
const TAIL: u8 = 64;

#[derive(Clone, Copy, PartialEq)]
pub struct Cell {
    pub ch: char,
    pub fg: u32,
    pub bg: u32,
    pub flags: u8,
}

impl Cell {
    fn blank(bg: u32) -> Cell {
        Cell { ch: ' ', fg: default_fg(), bg, flags: 0 }
    }
}

#[derive(Clone, Copy)]
pub struct Cursor {
    pub x: usize,
    pub y: usize,
    fg: u32,
    bg: u32,
    flags: u8,
    wrap_next: bool,
    gfx: bool,
    origin: bool,
}

impl Cursor {
    fn new() -> Cursor {
        Cursor { x: 0, y: 0, fg: default_fg(), bg: default_bg(), flags: 0, wrap_next: false, gfx: false, origin: false }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Ground,
    Esc,
    Csi,
    Osc,
    Str,
    Inter(u8),
}

pub struct Term {
    pub cols: usize,
    pub rows: usize,
    pub cur: Cursor,
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    pub cursor_visible: bool,
    pub mouse: u8,
    pub mouse_sgr: bool,
    pub cursor_style: Option<CursorStyle>,
    pub scrolled: bool,
    pub title: Option<String>,
    pub reply: Vec<u8>,
    pub sync: bool,
    pub focus_events: bool,
    pub cwd: Option<String>,
    pub clipboard: Option<String>,
    grid: Vec<Cell>,
    alt: Vec<Cell>,
    pub in_alt: bool,
    pub view: usize,
    history: VecDeque<Vec<Cell>>,
    dropped: usize,
    marks: Vec<usize>,
    saved: Cursor,
    top: usize,
    bot: usize,
    tabs: Vec<bool>,
    autowrap: bool,
    last: char,
    state: State,
    params: Vec<usize>,
    private: u8,
    inter: u8,
    osc: Vec<u8>,
    utf8: [u8; 4],
    utf8_len: usize,
    utf8_need: usize,
}

fn tab_stops(cols: usize) -> Vec<bool> {
    (0..cols).map(|x| x % 8 == 0).collect()
}

fn color256(n: usize) -> u32 {
    match n {
        0..=15 => palette(n),
        16..=231 => {
            let n = n - 16;
            let level = |v: usize| -> u32 { if v == 0 { 0 } else { 55 + 40 * v as u32 } };
            level(n / 36) << 16 | level(n / 6 % 6) << 8 | level(n % 6)
        }
        _ => {
            let v = 8 + 10 * (n.min(255) - 232) as u32;
            v << 16 | v << 8 | v
        }
    }
}

fn fold(ch: char, ignore_case: bool) -> char {
    if ignore_case { ch.to_lowercase().next().unwrap_or(ch) } else { ch }
}

fn decode_base64(data: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0);
    for b in data.bytes().filter(|&b| b != b'=') {
        let value = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = acc << 6 | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn parse_cwd(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let path = &rest[rest.find('/')?..];
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let Some(byte) = path.get(i + 1..i + 3).and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

fn line_drawing(ch: char) -> char {
    match ch {
        '`' => '◆', 'a' => '▒', 'f' => '°', 'g' => '±', 'j' => '┘', 'k' => '┐', 'l' => '┌',
        'm' => '└', 'n' => '┼', 'o' => '⎺', 'p' => '⎻', 'q' => '─', 'r' => '⎼', 's' => '⎽',
        't' => '├', 'u' => '┤', 'v' => '┴', 'w' => '┬', 'x' => '│', 'y' => '≤', 'z' => '≥',
        '{' => 'π', '|' => '≠', '}' => '£', '~' => '·',
        _ => ch,
    }
}

impl Term {
    pub fn new(cols: usize, rows: usize) -> Term {
        let blank = Cell::blank(default_bg());
        Term {
            cols,
            rows,
            cur: Cursor::new(),
            app_cursor: false,
            bracketed_paste: false,
            cursor_visible: true,
            mouse: 0,
            mouse_sgr: false,
            cursor_style: None,
            scrolled: false,
            title: None,
            reply: Vec::new(),
            sync: false,
            focus_events: false,
            cwd: None,
            clipboard: None,
            grid: vec![blank; cols * rows],
            alt: vec![blank; cols * rows],
            in_alt: false,
            view: 0,
            history: VecDeque::new(),
            dropped: 0,
            marks: Vec::new(),
            saved: Cursor::new(),
            top: 0,
            bot: rows - 1,
            tabs: tab_stops(cols),
            autowrap: true,
            last: ' ',
            state: State::Ground,
            params: vec![0],
            private: 0,
            inter: 0,
            osc: Vec::new(),
            utf8: [0; 4],
            utf8_len: 0,
            utf8_need: 0,
        }
    }

    /// The cells of viewport row `y`, which comes from the history while scrolled back.
    fn row_cells(&self, y: usize) -> &[Cell] {
        let past = self.history.len();
        let line = past - self.view.min(past) + y;
        if line < past {
            &self.history[line]
        } else {
            let y = line - past;
            &self.grid[y * self.cols..(y + 1) * self.cols]
        }
    }

    pub fn cell(&self, x: usize, y: usize) -> Cell {
        self.row_cells(y)[x]
    }

    /// Moves the view `delta` lines back into the history (negative: towards the live screen).
    /// Returns whether the view moved. The alternate screen has no history.
    pub fn scroll_view(&mut self, delta: isize) -> bool {
        if self.in_alt {
            return false;
        }
        let view = (self.view as isize).saturating_add(delta).clamp(0, self.history.len() as isize) as usize;
        let moved = view != self.view;
        self.view = view;
        moved
    }

    /// Jumps back to the live screen. Returns whether the view moved.
    pub fn reset_view(&mut self) -> bool {
        let moved = self.view != 0;
        self.view = 0;
        moved
    }

    fn push_history(&mut self, row: Vec<Cell>) {
        let max = config::get().scrollback.lines;
        if max == 0 {
            return;
        }
        if self.history.len() >= max {
            self.history.pop_front();
            self.dropped += 1;
        }
        self.history.push_back(row);
        // Keep the lines on screen in place while the user is reading older output.
        if self.view > 0 {
            self.view = (self.view + 1).min(self.history.len());
        }
    }

    /// Scrolls the whole scroll region up, keeping the lines that leave the top of the screen.
    fn scroll_region_up(&mut self, n: usize) {
        if !self.in_alt && self.top == 0 {
            let c = self.cols;
            for y in 0..n.min(self.bot + 1) {
                let row = self.grid[y * c..(y + 1) * c].to_vec();
                self.push_history(row);
            }
        }
        self.scroll_up(self.top, n);
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        if cols == self.cols && rows == self.rows {
            return;
        }
        let shift = (self.cur.y + 1).saturating_sub(rows);
        let blank = Cell::blank(default_bg());
        let reflow = |old: &[Cell], old_cols: usize, old_rows: usize| {
            let mut grid = vec![blank; cols * rows];
            for y in 0..rows.min(old_rows.saturating_sub(shift)) {
                for x in 0..cols.min(old_cols) {
                    grid[y * cols + x] = old[(y + shift) * old_cols + x];
                }
            }
            grid
        };
        if shift > 0 && !self.in_alt {
            for y in 0..shift.min(self.rows) {
                let row = self.grid[y * self.cols..(y + 1) * self.cols].to_vec();
                self.push_history(row);
            }
        }
        self.grid = reflow(&self.grid, self.cols, self.rows);
        self.alt = reflow(&self.alt, self.cols, self.rows);
        self.cols = cols;
        self.rows = rows;
        self.cur.y -= shift;
        self.cur.x = self.cur.x.min(cols - 1);
        self.cur.wrap_next = false;
        self.saved.x = self.saved.x.min(cols - 1);
        self.saved.y = self.saved.y.min(rows - 1);
        self.top = 0;
        self.bot = rows - 1;
        self.tabs = tab_stops(cols);
        for row in &mut self.history {
            row.resize(cols, blank);
        }
        self.view = self.view.min(self.history.len());
        self.scrolled = true;
    }

    pub fn text(&self, a: (usize, usize), b: (usize, usize)) -> String {
        let mut out = String::new();
        for y in a.1..=b.1 {
            let x1 = if y == a.1 { a.0 } else { 0 };
            let x2 = if y == b.1 { b.0 } else { self.cols - 1 };
            let row = self.row_cells(y);
            let mut line: String = row[x1..=x2].iter().filter(|c| c.flags & TAIL == 0).map(|c| c.ch).collect();
            if x2 == self.cols - 1 && row[x2].flags & WRAP == 0 {
                line.truncate(line.trim_end().len());
                if y != b.1 {
                    line.push('\n');
                }
            }
            out += &line;
        }
        out
    }

    pub fn word_at(&self, x: usize, y: usize) -> (usize, usize) {
        let row = self.row_cells(y);
        let delim = |c: char| config::get().mouse.word_delims.contains(c);
        if delim(row[x].ch) {
            return (x, x);
        }
        let mut start = x;
        while start > 0 && !delim(row[start - 1].ch) {
            start -= 1;
        }
        let mut end = x;
        while end + 1 < self.cols && !delim(row[end + 1].ch) {
            end += 1;
        }
        (start, end)
    }

    pub fn lines(&self) -> usize {
        self.history.len() + self.rows
    }

    fn line_cells(&self, line: usize) -> &[Cell] {
        let past = self.history.len();
        if line < past {
            &self.history[line]
        } else {
            let y = line - past;
            &self.grid[y * self.cols..(y + 1) * self.cols]
        }
    }

    fn row_matches(&self, line: usize, needle: &[char], ignore_case: bool) -> Vec<(usize, usize)> {
        let (chars, cols): (Vec<char>, Vec<usize>) = self
            .line_cells(line)
            .iter()
            .enumerate()
            .filter(|(_, cell)| cell.flags & TAIL == 0)
            .map(|(x, cell)| (fold(cell.ch, ignore_case), x))
            .unzip();
        if needle.is_empty() || needle.len() > chars.len() {
            return Vec::new();
        }
        (0..=chars.len() - needle.len())
            .filter(|&i| chars[i..i + needle.len()] == *needle)
            .map(|i| {
                let last = cols[i + needle.len() - 1];
                let wide = self.line_cells(line).get(last + 1).is_some_and(|c| c.flags & TAIL != 0);
                (cols[i], last + wide as usize)
            })
            .collect()
    }

    pub fn find(&self, query: &str, from: (usize, usize), backward: bool) -> Option<(usize, usize, usize)> {
        let ignore_case = !query.chars().any(char::is_uppercase);
        let needle: Vec<char> = query.chars().map(|c| fold(c, ignore_case)).collect();
        let total = self.lines();
        let order: Box<dyn Iterator<Item = usize>> = if backward {
            Box::new((0..=from.0.min(total - 1)).rev())
        } else {
            Box::new(from.0..total)
        };
        for line in order {
            let mut found = self.row_matches(line, &needle, ignore_case).into_iter();
            let hit = if backward {
                found.rev().find(|m| line < from.0 || m.0 <= from.1)
            } else {
                found.find(|m| line > from.0 || m.0 >= from.1)
            };
            if let Some((start, end)) = hit {
                return Some((line, start, end));
            }
        }
        None
    }

    pub fn reveal(&mut self, line: usize) -> usize {
        let past = self.history.len();
        let top = past - self.view;
        if line < top || line >= top + self.rows {
            self.view = past.saturating_sub(line.saturating_sub(self.rows / 2));
        }
        line - (past - self.view)
    }

    pub fn jump_prompt(&mut self, back: bool) -> bool {
        if self.in_alt {
            return false;
        }
        let past = self.history.len();
        let top = self.dropped + past - self.view;
        self.marks.retain(|&mark| mark >= self.dropped);
        let target = if back {
            self.marks.iter().rev().find(|&&mark| mark < top)
        } else {
            self.marks.iter().find(|&&mark| mark > top)
        };
        let view = match target {
            Some(&mark) if mark - self.dropped < past => past - (mark - self.dropped),
            Some(_) => 0,
            None if back => return false,
            None => 0,
        };
        let moved = view != self.view;
        self.view = view;
        moved
    }

    pub fn url_at(&self, x: usize, y: usize) -> Option<String> {
        let row = self.row_cells(y);
        let stop = |c: char| c.is_whitespace() || "<>\"'`".contains(c);
        if stop(row[x].ch) {
            return None;
        }
        let mut start = x;
        while start > 0 && !stop(row[start - 1].ch) {
            start -= 1;
        }
        let mut end = x;
        while end + 1 < self.cols && !stop(row[end + 1].ch) {
            end += 1;
        }
        let token: String = row[start..=end].iter().filter(|c| c.flags & TAIL == 0).map(|c| c.ch).collect();
        let mut url = token.trim_start_matches(['(', '[', '{']);
        while let Some(last) = url.chars().last() {
            let count = |c: char| url.matches(c).count();
            let trim = match last {
                ')' => count('(') < count(')'),
                ']' => count('[') < count(']'),
                '}' => count('{') < count('}'),
                '.' | ',' | ';' | ':' | '!' | '?' => true,
                _ => false,
            };
            if !trim {
                break;
            }
            url = &url[..url.len() - last.len_utf8()];
        }
        ["http://", "https://", "ftp://", "file://", "mailto:"]
            .iter()
            .any(|scheme| url.starts_with(scheme))
            .then(|| url.to_string())
    }

    pub fn feed(&mut self, data: &[u8]) {
        for &b in data {
            self.byte(b);
        }
    }

    fn byte(&mut self, b: u8) {
        if matches!(self.state, State::Osc | State::Str) {
            return self.string(b);
        }
        match b {
            0x1b => {
                self.state = State::Esc;
                self.utf8_need = 0;
            }
            0x18 | 0x1a => self.state = State::Ground,
            _ => match self.state {
                State::Ground => self.ground(b),
                _ if b < 0x20 => self.control(b),
                State::Esc => self.esc(b),
                State::Csi => self.csi_byte(b),
                State::Inter(i) => {
                    self.state = State::Ground;
                    self.intermediate(i, b);
                }
                _ => {}
            },
        }
    }

    fn ground(&mut self, b: u8) {
        if b < 0x80 {
            if self.utf8_need > 0 {
                self.utf8_need = 0;
                self.put('\u{fffd}');
            }
            match b {
                0x20..=0x7e => self.put(b as char),
                0x7f => {}
                _ => self.control(b),
            }
        } else if b & 0xc0 == 0x80 {
            if self.utf8_need == 0 {
                return self.put('\u{fffd}');
            }
            self.utf8[self.utf8_len] = b;
            self.utf8_len += 1;
            if self.utf8_len == self.utf8_need {
                self.utf8_need = 0;
                let ch = std::str::from_utf8(&self.utf8[..self.utf8_len]).ok().and_then(|s| s.chars().next());
                self.put(ch.unwrap_or('\u{fffd}'));
            }
        } else {
            if self.utf8_need > 0 {
                self.put('\u{fffd}');
            }
            self.utf8[0] = b;
            self.utf8_len = 1;
            self.utf8_need = match b {
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => 0,
            };
            if self.utf8_need == 0 {
                self.put('\u{fffd}');
            }
        }
    }

    fn control(&mut self, b: u8) {
        match b {
            0x08 => {
                self.cur.x = self.cur.x.saturating_sub(1);
                self.cur.wrap_next = false;
            }
            0x09 => self.tab(1),
            0x0a..=0x0c => self.linefeed(),
            0x0d => {
                self.cur.x = 0;
                self.cur.wrap_next = false;
            }
            _ => {}
        }
    }

    fn string(&mut self, b: u8) {
        match b {
            0x07 | 0x1b => {
                if self.state == State::Osc {
                    self.finish_osc();
                }
                self.state = if b == 0x1b { State::Esc } else { State::Ground };
            }
            _ if self.state == State::Osc && self.osc.len() < 1 << 20 => self.osc.push(b),
            _ => {}
        }
    }

    fn finish_osc(&mut self) {
        let text = String::from_utf8_lossy(&self.osc).into_owned();
        let Some((cmd, rest)) = text.split_once(';') else { return };
        match cmd {
            "0" | "2" => self.title = Some(rest.to_string()),
            "7" => self.cwd = parse_cwd(rest),
            "52" => {
                let data = rest.split_once(';').map_or("", |(_, data)| data);
                if let Some(text) = decode_base64(data).and_then(|bytes| String::from_utf8(bytes).ok()) {
                    self.clipboard = Some(text);
                }
            }
            "133" if rest.starts_with('A') && !self.in_alt => {
                let line = self.dropped + self.history.len() + self.cur.y;
                self.marks.retain(|&mark| mark >= self.dropped);
                if self.marks.last() != Some(&line) {
                    self.marks.push(line);
                }
            }
            _ => {}
        }
    }

    fn esc(&mut self, b: u8) {
        self.state = State::Ground;
        match b {
            b'[' => {
                self.params.clear();
                self.params.push(0);
                self.private = 0;
                self.inter = 0;
                self.state = State::Csi;
            }
            b']' => {
                self.osc.clear();
                self.state = State::Osc;
            }
            b'P' | b'_' | b'^' | b'X' => self.state = State::Str,
            b'(' | b')' | b'*' | b'+' | b'#' | b'%' => self.state = State::Inter(b),
            b'7' => self.saved = self.cur,
            b'8' => self.restore(),
            b'D' => self.linefeed(),
            b'E' => {
                self.cur.x = 0;
                self.linefeed();
            }
            b'M' => {
                if self.cur.y == self.top {
                    self.scroll_down(self.top, 1);
                } else if self.cur.y > 0 {
                    self.cur.y -= 1;
                }
            }
            b'H' => self.tabs[self.cur.x] = true,
            b'c' => self.reset(),
            _ => {}
        }
    }

    fn intermediate(&mut self, inter: u8, b: u8) {
        match (inter, b) {
            (b'(', _) => self.cur.gfx = b == b'0',
            (b'#', b'8') => self.grid.fill(Cell { ch: 'E', ..Cell::blank(default_bg()) }),
            _ => {}
        }
    }

    fn csi_byte(&mut self, b: u8) {
        match b {
            b'0'..=b'9' => {
                let p = self.params.last_mut().unwrap();
                *p = (*p * 10 + (b - b'0') as usize).min(65535);
            }
            b';' | b':' => {
                if self.params.len() < 32 {
                    self.params.push(0);
                }
            }
            b'<'..=b'?' => self.private = b,
            b' '..=b'/' => self.inter = b,
            b'@'..=b'~' => {
                self.state = State::Ground;
                self.csi(b);
            }
            _ => {}
        }
    }

    fn arg(&self, i: usize, default: usize) -> usize {
        match self.params.get(i) {
            Some(&v) if v > 0 => v,
            _ => default,
        }
    }

    fn csi(&mut self, f: u8) {
        match (self.private, self.inter, f) {
            (b'?', 0, b'h') => self.dec_mode(true),
            (b'?', 0, b'l') => self.dec_mode(false),
            (b'>', 0, b'c') => self.reply.extend_from_slice(b"\x1b[>0;0;0c"),
            (0, b'!', b'p') => self.reset(),
            (0, b' ', b'q') => {
                self.cursor_style = match self.params[0] {
                    0 => None,
                    1 | 2 => Some(CursorStyle::Block),
                    3 | 4 => Some(CursorStyle::Underline),
                    5 | 6 => Some(CursorStyle::Beam),
                    _ => self.cursor_style,
                }
            }
            (0, 0, _) => self.csi_plain(f),
            _ => {}
        }
    }

    fn csi_plain(&mut self, f: u8) {
        let (x, y, n) = (self.cur.x, self.cur.y, self.arg(0, 1));
        let low = if y >= self.top { self.top } else { 0 };
        let high = if y <= self.bot { self.bot } else { self.rows - 1 };
        match f {
            b'@' => self.insert_chars(n),
            b'A' => self.goto(x, y.saturating_sub(n).max(low)),
            b'B' => self.goto(x, (y + n).min(high)),
            b'C' => self.goto(x + n, y),
            b'D' => self.goto(x.saturating_sub(n), y),
            b'E' => self.goto(0, (y + n).min(high)),
            b'F' => self.goto(0, y.saturating_sub(n).max(low)),
            b'G' | b'`' => self.goto(n - 1, y),
            b'H' | b'f' => self.goto(self.arg(1, 1) - 1, self.arg(0, 1) - 1 + self.row_offset()),
            b'I' => self.tab(n as isize),
            b'Z' => self.tab(-(n as isize)),
            b'J' => match self.params[0] {
                0 => {
                    self.erase(y, x, self.cols);
                    for row in y + 1..self.rows {
                        self.erase(row, 0, self.cols);
                    }
                }
                1 => {
                    for row in 0..y {
                        self.erase(row, 0, self.cols);
                    }
                    self.erase(y, 0, x + 1);
                }
                3 => {
                    self.dropped += self.history.len();
                    self.history.clear();
                    self.view = 0;
                    self.scrolled = true;
                }
                _ => {
                    for row in 0..self.rows {
                        self.erase(row, 0, self.cols);
                    }
                }
            },
            b'K' => match self.params[0] {
                0 => self.erase(y, x, self.cols),
                1 => self.erase(y, 0, x + 1),
                _ => self.erase(y, 0, self.cols),
            },
            b'L' => {
                if y >= self.top && y <= self.bot {
                    self.scroll_down(y, n);
                    self.goto(0, y);
                }
            }
            b'M' => {
                if y >= self.top && y <= self.bot {
                    self.scroll_up(y, n);
                    self.goto(0, y);
                }
            }
            b'P' => self.delete_chars(n),
            b'X' => self.erase(y, x, (x + n).min(self.cols)),
            b'S' => self.scroll_region_up(n),
            b'T' => self.scroll_down(self.top, n),
            b'b' => {
                for _ in 0..n {
                    self.put(self.last);
                }
            }
            b'd' => self.goto(x, n - 1 + self.row_offset()),
            b'g' => match self.params[0] {
                0 => self.tabs[x] = false,
                3 => self.tabs.fill(false),
                _ => {}
            },
            b'm' => self.sgr(),
            b'n' => match self.params[0] {
                5 => self.reply.extend_from_slice(b"\x1b[0n"),
                6 => {
                    let row = y + 1 - self.row_offset();
                    self.reply.extend_from_slice(format!("\x1b[{};{}R", row, x + 1).as_bytes());
                }
                _ => {}
            },
            b'c' => self.reply.extend_from_slice(b"\x1b[?6c"),
            b'r' => {
                let (top, bot) = (self.arg(0, 1) - 1, self.arg(1, self.rows) - 1);
                if top < bot && bot < self.rows {
                    self.top = top;
                    self.bot = bot;
                    self.goto(0, self.row_offset());
                }
            }
            b's' => self.saved = self.cur,
            b'u' => self.restore(),
            _ => {}
        }
    }

    fn dec_mode(&mut self, set: bool) {
        for p in std::mem::take(&mut self.params) {
            match p {
                1 => self.app_cursor = set,
                6 => {
                    self.cur.origin = set;
                    self.goto(0, self.row_offset());
                }
                7 => self.autowrap = set,
                25 => self.cursor_visible = set,
                1000 => self.mouse = if set { 1 } else { 0 },
                1002 => self.mouse = if set { 2 } else { 0 },
                1003 => self.mouse = if set { 3 } else { 0 },
                1006 => self.mouse_sgr = set,
                47 | 1047 => self.alt_screen(set, false),
                1048 => {
                    if set {
                        self.saved = self.cur;
                    } else {
                        self.restore();
                    }
                }
                1049 => self.alt_screen(set, true),
                2004 => self.bracketed_paste = set,
                1004 => self.focus_events = set,
                2026 => self.sync = set,
                _ => {}
            }
        }
    }

    fn alt_screen(&mut self, on: bool, save_cursor: bool) {
        if on == self.in_alt {
            return;
        }
        if on && save_cursor {
            self.saved = self.cur;
        }
        std::mem::swap(&mut self.grid, &mut self.alt);
        self.in_alt = on;
        self.view = 0;
        if on {
            self.grid.fill(Cell::blank(default_bg()));
        } else if save_cursor {
            self.restore();
        }
        self.scrolled = true;
    }

    fn sgr(&mut self) {
        let p = std::mem::take(&mut self.params);
        let mut i = 0;
        while i < p.len() {
            match p[i] {
                0 => {
                    self.cur.fg = default_fg();
                    self.cur.bg = default_bg();
                    self.cur.flags = 0;
                }
                1 => self.cur.flags |= BOLD,
                2 => self.cur.flags |= DIM,
                9 => self.cur.flags |= STRIKE,
                4 => self.cur.flags |= UNDERLINE,
                7 => self.cur.flags |= REVERSE,
                22 => self.cur.flags &= !(BOLD | DIM),
                29 => self.cur.flags &= !STRIKE,
                24 => self.cur.flags &= !UNDERLINE,
                27 => self.cur.flags &= !REVERSE,
                30..=37 => self.cur.fg = palette(p[i] - 30),
                40..=47 => self.cur.bg = palette(p[i] - 40),
                90..=97 => self.cur.fg = palette(p[i] - 90 + 8),
                100..=107 => self.cur.bg = palette(p[i] - 100 + 8),
                39 => self.cur.fg = default_fg(),
                49 => self.cur.bg = default_bg(),
                38 | 48 => {
                    let (color, used) = match (p.get(i + 1), p.get(i + 2), p.get(i + 3), p.get(i + 4)) {
                        (Some(5), Some(&n), _, _) => (Some(color256(n)), 2),
                        (Some(2), Some(&r), Some(&g), Some(&b)) => {
                            (Some((r.min(255) as u32) << 16 | (g.min(255) as u32) << 8 | b.min(255) as u32), 4)
                        }
                        _ => (None, p.len()),
                    };
                    if let Some(c) = color {
                        if p[i] == 38 {
                            self.cur.fg = c;
                        } else {
                            self.cur.bg = c;
                        }
                    }
                    i += used;
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn row_offset(&self) -> usize {
        if self.cur.origin { self.top } else { 0 }
    }

    fn goto(&mut self, x: usize, y: usize) {
        let (low, high) = if self.cur.origin { (self.top, self.bot) } else { (0, self.rows - 1) };
        self.cur.x = x.min(self.cols - 1);
        self.cur.y = y.clamp(low, high);
        self.cur.wrap_next = false;
    }

    fn restore(&mut self) {
        self.cur = self.saved;
        self.cur.x = self.cur.x.min(self.cols - 1);
        self.cur.y = self.cur.y.min(self.rows - 1);
    }

    fn tab(&mut self, n: isize) {
        let mut x = self.cur.x;
        for _ in 0..n.abs() {
            if n > 0 {
                x += 1;
                while x < self.cols - 1 && !self.tabs[x] {
                    x += 1;
                }
            } else {
                x = x.saturating_sub(1);
                while x > 0 && !self.tabs[x] {
                    x -= 1;
                }
            }
            x = x.min(self.cols - 1);
        }
        self.cur.x = x;
        self.cur.wrap_next = false;
    }

    fn linefeed(&mut self) {
        if self.cur.y == self.bot {
            self.scroll_region_up(1);
        } else if self.cur.y + 1 < self.rows {
            self.cur.y += 1;
        }
        self.cur.wrap_next = false;
    }

    fn put(&mut self, ch: char) {
        let width = if self.cur.gfx { 1 } else { ch.width().unwrap_or(1) };
        if width == 0 {
            return;
        }
        let ch = if self.cur.gfx { line_drawing(ch) } else { ch };
        if self.cur.wrap_next && self.autowrap {
            let i = self.cur.y * self.cols + self.cols - 1;
            self.grid[i].flags |= WRAP;
            self.cur.x = 0;
            self.linefeed();
        }
        if width == 2 && self.cur.x + 1 >= self.cols {
            if !self.autowrap || self.cols < 2 {
                return;
            }
            let i = self.cur.y * self.cols + self.cur.x;
            self.grid[i] = Cell { ch: ' ', flags: WRAP, ..Cell::blank(self.cur.bg) };
            self.cur.x = 0;
            self.linefeed();
        }
        let i = self.cur.y * self.cols + self.cur.x;
        let Cursor { fg, bg, flags, .. } = self.cur;
        self.grid[i] = Cell { ch, fg, bg, flags };
        if width == 2 {
            self.grid[i + 1] = Cell { ch: ' ', fg, bg, flags: flags | TAIL };
        }
        self.last = ch;
        if self.cur.x + width < self.cols {
            self.cur.x += width;
            self.cur.wrap_next = false;
        } else {
            self.cur.x = self.cols - 1;
            self.cur.wrap_next = true;
        }
    }

    fn erase(&mut self, y: usize, x1: usize, x2: usize) {
        let blank = Cell::blank(self.cur.bg);
        let row = y * self.cols;
        self.grid[row + x1..row + x2].fill(blank);
    }

    fn scroll_up(&mut self, from: usize, n: usize) {
        if from > self.bot {
            return;
        }
        let n = n.min(self.bot + 1 - from);
        let c = self.cols;
        self.grid.copy_within((from + n) * c..(self.bot + 1) * c, from * c);
        for y in self.bot + 1 - n..=self.bot {
            self.erase(y, 0, c);
        }
        self.scrolled = true;
    }

    fn scroll_down(&mut self, from: usize, n: usize) {
        if from > self.bot {
            return;
        }
        let n = n.min(self.bot + 1 - from);
        let c = self.cols;
        self.grid.copy_within(from * c..(self.bot + 1 - n) * c, (from + n) * c);
        for y in from..from + n {
            self.erase(y, 0, c);
        }
        self.scrolled = true;
    }

    fn insert_chars(&mut self, n: usize) {
        let (x, y, c) = (self.cur.x, self.cur.y, self.cols);
        let n = n.min(c - x);
        self.grid.copy_within(y * c + x..y * c + c - n, y * c + x + n);
        self.erase(y, x, x + n);
    }

    fn delete_chars(&mut self, n: usize) {
        let (x, y, c) = (self.cur.x, self.cur.y, self.cols);
        let n = n.min(c - x);
        self.grid.copy_within(y * c + x + n..y * c + c, y * c + x);
        self.erase(y, c - n, c);
    }

    fn reset(&mut self) {
        let reply = std::mem::take(&mut self.reply);
        *self = Term::new(self.cols, self.rows);
        self.reply = reply;
        self.scrolled = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(t: &Term, y: usize) -> String {
        (0..t.cols).map(|x| t.cell(x, y).ch).collect::<String>().trim_end().to_string()
    }

    #[test]
    fn prints_and_wraps() {
        let mut t = Term::new(5, 3);
        t.feed(b"abcdefg");
        assert_eq!(row(&t, 0), "abcde");
        assert_eq!(row(&t, 1), "fg");
        assert_eq!(t.text((0, 0), (4, 1)), "abcdefg");
    }

    #[test]
    fn keeps_scrolled_off_lines_in_history() {
        let mut t = Term::new(6, 2);
        t.feed(b"one\r\ntwo\r\nthree\r\nfour");
        assert_eq!(t.history.len(), 2);
        assert!(t.scroll_view(1));
        assert_eq!(row(&t, 0), "two");
        assert_eq!(row(&t, 1), "three");
        assert!(t.scroll_view(5));
        assert_eq!(row(&t, 0), "one");
        assert_eq!(row(&t, 1), "two");
        assert!(t.scroll_view(-9));
        assert_eq!(row(&t, 0), "three");
        assert_eq!(row(&t, 1), "four");
    }

    #[test]
    fn view_stays_put_while_output_arrives() {
        let mut t = Term::new(6, 2);
        t.feed(b"one\r\ntwo\r\nthree");
        t.scroll_view(1);
        assert_eq!(row(&t, 0), "one");
        t.feed(b"\r\nfour");
        assert_eq!(row(&t, 0), "one");
        assert!(t.reset_view());
        assert_eq!(row(&t, 1), "four");
    }

    #[test]
    fn alt_screen_has_no_history() {
        let mut t = Term::new(6, 2);
        t.feed(b"\x1b[?1049ha\r\nb\r\nc\r\nd");
        assert_eq!(t.history.len(), 0);
        assert!(!t.scroll_view(1));
    }

    #[test]
    fn erase_saved_lines_clears_history() {
        let mut t = Term::new(6, 2);
        t.feed(b"one\r\ntwo\r\nthree");
        assert_eq!(t.history.len(), 1);
        t.feed(b"\x1b[3J");
        assert_eq!(t.history.len(), 0);
    }

    #[test]
    fn scrolls_at_bottom() {
        let mut t = Term::new(6, 2);
        t.feed(b"one\r\ntwo\r\nthree");
        assert_eq!(row(&t, 0), "two");
        assert_eq!(row(&t, 1), "three");
    }

    #[test]
    fn cursor_movement_and_erase() {
        let mut t = Term::new(10, 3);
        t.feed(b"hello\x1b[3D\x1b[K");
        assert_eq!(row(&t, 0), "he");
        t.feed(b"\x1b[2;4Hx");
        assert_eq!((t.cur.x, t.cur.y), (4, 1));
        assert_eq!(t.cell(3, 1).ch, 'x');
    }

    #[test]
    fn colors() {
        let mut t = Term::new(10, 2);
        t.feed(b"\x1b[38;2;1;2;3mA\x1b[48;5;196mB\x1b[0mC");
        assert_eq!(t.cell(0, 0).fg, 0x010203);
        assert_eq!(t.cell(1, 0).bg, 0xff0000);
        assert_eq!(t.cell(2, 0).bg, default_bg());
    }

    #[test]
    fn utf8_and_title() {
        let mut t = Term::new(10, 2);
        t.feed("é€😀".as_bytes());
        t.feed(b"\x1b]0;hi there\x07");
        assert_eq!(row(&t, 0), "é€😀");
        assert_eq!(t.title.as_deref(), Some("hi there"));
    }

    #[test]
    fn alt_screen_restores() {
        let mut t = Term::new(6, 2);
        t.feed(b"main\x1b[?1049halt\x1b[?1049l");
        assert_eq!(row(&t, 0), "main");
        assert_eq!(t.cur.x, 4);
    }

    #[test]
    fn scroll_region_and_insert_line() {
        let mut t = Term::new(4, 4);
        t.feed(b"a\r\nb\r\nc\r\nd\x1b[2;3r\x1b[2;1H\x1b[L");
        assert_eq!((row(&t, 0), row(&t, 1), row(&t, 2), row(&t, 3)), ("a".into(), "".into(), "b".into(), "d".into()));
    }

    #[test]
    fn device_status_report() {
        let mut t = Term::new(10, 5);
        t.feed(b"\x1b[3;4H\x1b[6n");
        assert_eq!(t.reply, b"\x1b[3;4R");
    }

    #[test]
    fn resize_keeps_cursor_line() {
        let mut t = Term::new(5, 4);
        t.feed(b"1\r\n2\r\n3\r\n4");
        t.resize(5, 2);
        assert_eq!((row(&t, 0), row(&t, 1)), ("3".into(), "4".into()));
        assert_eq!(t.cur.y, 1);
    }

    #[test]
    fn mouse_modes() {
        let mut t = Term::new(10, 2);
        t.feed(b"\x1b[?1002h\x1b[?1006h");
        assert_eq!((t.mouse, t.mouse_sgr), (2, true));
        t.feed(b"\x1b[?1002l");
        assert_eq!(t.mouse, 0);
    }

    #[test]
    fn cursor_shape() {
        let mut t = Term::new(10, 2);
        t.feed(b"\x1b[5 q");
        assert!(matches!(t.cursor_style, Some(CursorStyle::Beam)));
        t.feed(b"\x1b[0 q");
        assert!(t.cursor_style.is_none());
    }

    #[test]
    fn wide_chars_take_two_cells() {
        let mut t = Term::new(6, 2);
        t.feed("a日b".as_bytes());
        assert_eq!((t.cell(1, 0).ch, t.cell(3, 0).ch, t.cur.x), ('日', 'b', 4));
        assert_eq!(t.text((0, 0), (5, 0)), "a日b");
        let mut t = Term::new(4, 2);
        t.feed("abc日".as_bytes());
        assert_eq!((t.cell(0, 1).ch, t.cur.y, t.cur.x), ('日', 1, 2));
    }

    #[test]
    fn osc_52_and_7() {
        let mut t = Term::new(10, 2);
        t.feed(b"\x1b]52;c;aGVsbG8=\x07\x1b]7;file://host/tmp/a%20b\x07");
        assert_eq!(t.clipboard.as_deref(), Some("hello"));
        assert_eq!(t.cwd.as_deref(), Some("/tmp/a b"));
    }

    #[test]
    fn sync_and_focus_modes() {
        let mut t = Term::new(10, 2);
        t.feed(b"\x1b[?2026h\x1b[?1004h");
        assert!(t.sync && t.focus_events);
        t.feed(b"\x1b[?2026l");
        assert!(!t.sync);
    }

    #[test]
    fn dim_and_strike() {
        let mut t = Term::new(10, 2);
        t.feed(b"\x1b[2;9mA\x1b[22;29mB");
        assert_eq!(t.cell(0, 0).flags & (DIM | STRIKE), DIM | STRIKE);
        assert_eq!(t.cell(1, 0).flags & (DIM | STRIKE), 0);
    }

    #[test]
    fn jumps_between_prompts() {
        let mut t = Term::new(6, 2);
        t.feed(b"\x1b]133;A\x07one\r\ntwo\r\n\x1b]133;A\x07three\r\nfour");
        assert!(t.jump_prompt(true));
        assert_eq!(row(&t, 0), "one");
        assert!(t.jump_prompt(false));
        assert_eq!(t.view, 0);
        assert!(!t.jump_prompt(false));
    }

    #[test]
    fn finds_text_in_both_directions() {
        let mut t = Term::new(10, 2);
        t.feed(b"foo bar\r\nBaz foo");
        assert_eq!(t.find("foo", (1, usize::MAX), true), Some((1, 4, 6)));
        assert_eq!(t.find("foo", (1, 3), true), Some((0, 0, 2)));
        assert_eq!(t.find("foo", (0, 1), false), Some((1, 4, 6)));
        assert_eq!(t.find("baz", (1, usize::MAX), true), Some((1, 0, 2)));
        assert_eq!(t.find("BAZ", (1, usize::MAX), true), None);
    }

    #[test]
    fn detects_urls() {
        let mut t = Term::new(40, 2);
        t.feed(b"see (https://a.b/c). ok");
        assert_eq!(t.url_at(10, 0).as_deref(), Some("https://a.b/c"));
        assert_eq!(t.url_at(1, 0), None);
    }
}
