#![allow(non_upper_case_globals)]

mod config;
mod font;
mod keys;
mod pty;
mod term;

use std::error::Error;
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use memmap2::{MmapMut, MmapOptions};
use wayland_client::backend::WaylandError;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_callback::{self, WlCallback};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::WlDataDeviceManager;
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::wl_data_source::{self, WlDataSource};
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{Shape, WpCursorShapeDeviceV1};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;
use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1;
use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_device_v1::{self, ZwpPrimarySelectionDeviceV1};
use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_offer_v1::{self, ZwpPrimarySelectionOfferV1};
use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_source_v1::{self, ZwpPrimarySelectionSourceV1};
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self, XdgToplevel};
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};
use xkbcommon::xkb;
use xkbcommon::xkb::keysyms::{KEY_BackSpace, KEY_Down, KEY_Escape, KEY_R, KEY_Return, KEY_S, KEY_Up, KEY_r, KEY_s};

use config::{Action, CursorStyle};
use font::{Bitmap, Fonts};
use pty::Pty;
use term::{Term, BOLD, DIM, REVERSE, STRIKE, UNDERLINE};

const BTN_LEFT: u32 = 0x110;
const BTN_MIDDLE: u32 = 0x112;
const BTN_RIGHT: u32 = 0x111;
const COPY_MIMES: [&str; 2] = ["text/plain;charset=utf-8", "text/plain"];
const PASTE_MIMES: [&str; 4] = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "STRING"];

type Pos = (usize, usize);

struct Buffer {
    buffer: WlBuffer,
    map: MmapMut,
    busy: bool,
}

impl Drop for Buffer {
    fn drop(&mut self) {
        self.buffer.destroy();
    }
}

struct Search {
    query: String,
    pos: Pos,
    found: bool,
}

#[derive(Clone, Copy)]
struct Repeat {
    key: u32,
    next: Instant,
}

struct App {
    shm: WlShm,
    surface: WlSurface,
    toplevel: XdgToplevel,
    data_manager: WlDataDeviceManager,
    data_device: WlDataDevice,
    cursor_manager: Option<WpCursorShapeManagerV1>,
    cursor_shape: Option<WpCursorShapeDeviceV1>,
    primary_manager: Option<ZwpPrimarySelectionDeviceManagerV1>,
    primary_device: Option<ZwpPrimarySelectionDeviceV1>,
    keyboard: Option<WlKeyboard>,
    pointer: Option<WlPointer>,

    xkb: xkb::Context,
    keymap: Option<xkb::Keymap>,
    xkb_state: Option<xkb::State>,
    repeat: Option<Repeat>,
    repeat_rate: u64,
    repeat_delay: u64,
    serial: u32,

    term: Term,
    pty: Pty,
    fonts: Fonts,

    width: u32,
    height: u32,
    pending: (u32, u32),
    configured: bool,
    buffers: Vec<Buffer>,
    dirty: bool,
    frame_pending: bool,
    focused: bool,

    mouse: (f64, f64),
    anchor: Option<Pos>,
    selection: Option<(Pos, Pos)>,
    last_click: Instant,
    clicks: u32,
    held: Option<u8>,
    last_cell: Pos,
    wheel: f64,

    clip_text: String,
    clip_source: Option<WlDataSource>,
    offer: Option<WlDataOffer>,
    offer_mimes: Vec<String>,
    pending_mimes: Vec<String>,
    paste: Option<File>,
    primary_text: String,
    primary_source: Option<ZwpPrimarySelectionSourceV1>,
    primary_offer: Option<ZwpPrimarySelectionOfferV1>,
    primary_mimes: Vec<String>,

    search: Option<Search>,
    sync_deadline: Option<Instant>,
    exit: bool,
}

fn border() -> usize {
    config::get().window.border
}

fn blend(dst: u32, src: u32, alpha: u32) -> u32 {
    let src = src | 0xff000000;
    let mix = |shift: u32| {
        let (s, d) = ((src >> shift) & 0xff, (dst >> shift) & 0xff);
        ((s * alpha + d * (255 - alpha) + 127) / 255) << shift
    };
    mix(24) | mix(16) | mix(8) | mix(0)
}

fn paint(color: u32) -> u32 {
    let alpha = (config::get().window.alpha.clamp(0.0, 1.0) * 255.0).round() as u32;
    if color != config::get().colors.background || alpha == 255 {
        return 0xff000000 | color;
    }
    let premultiply = |shift: u32| (((color >> shift) & 0xff) * alpha + 127) / 255 << shift;
    alpha << 24 | premultiply(16) | premultiply(8) | premultiply(0)
}

fn fill(px: &mut [u32], stride: usize, x: usize, y: usize, w: usize, h: usize, color: u32) {
    for row in y..y + h {
        px[row * stride + x..row * stride + x + w].fill(color);
    }
}

fn blit(px: &mut [u32], stride: usize, glyph: &Bitmap, (ox, oy): Pos, (right, bottom): Pos, fg: u32) {
    let (x0, y0) = (ox as i32 + glyph.x, oy as i32 + glyph.y);
    for gy in 0..glyph.height {
        for gx in 0..glyph.width {
            let alpha = glyph.alpha[gy * glyph.width + gx] as u32;
            let (tx, ty) = (x0 + gx as i32, y0 + gy as i32);
            if alpha == 0 || tx < ox as i32 || tx >= right as i32 || ty < oy as i32 || ty >= bottom as i32 {
                continue;
            }
            let p = &mut px[ty as usize * stride + tx as usize];
            *p = blend(*p, fg, alpha);
        }
    }
}

fn launch(command: &mut Command) {
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if let Ok(mut child) = command.spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

fn ordered(a: Pos, b: Pos) -> (Pos, Pos) {
    if (a.1, a.0) <= (b.1, b.0) { (a, b) } else { (b, a) }
}

fn selected(sel: Option<(Pos, Pos)>, x: usize, y: usize) -> bool {
    sel.is_some_and(|(a, b)| {
        let (a, b) = ordered(a, b);
        (y, x) >= (a.1, a.0) && (y, x) <= (b.1, b.0)
    })
}

fn make_buffer(shm: &WlShm, qh: &QueueHandle<App>, w: usize, h: usize, index: usize) -> Buffer {
    let size = w * h * 4;
    let fd = unsafe { libc::memfd_create(b"softty\0".as_ptr().cast(), libc::MFD_CLOEXEC) };
    let file = unsafe { File::from_raw_fd(fd) };
    file.set_len(size as u64).expect("cannot size shm file");
    let map = unsafe { MmapMut::map_mut(&file) }.expect("cannot map shm file");
    let pool = shm.create_pool(file.as_fd(), size as i32, qh, ());
    let buffer = pool.create_buffer(0, w as i32, h as i32, (w * 4) as i32, wl_shm::Format::Argb8888, qh, index);
    pool.destroy();
    Buffer { buffer, map, busy: false }
}

impl App {
    fn grid_size(&self) -> (usize, usize) {
        let usable = |px: u32, cell: usize| ((px as usize).saturating_sub(2 * border()) / cell).max(1);
        (usable(self.width, self.fonts.cell_w), usable(self.height, self.fonts.cell_h))
    }

    fn apply_size(&mut self, qh: &QueueHandle<App>) {
        let (mut w, mut h) = self.pending;
        if w == 0 || h == 0 {
            w = if self.width > 0 { self.width } else { (config::get().window.cols * self.fonts.cell_w + 2 * border()) as u32 };
            h = if self.height > 0 { self.height } else { (config::get().window.rows * self.fonts.cell_h + 2 * border()) as u32 };
        }
        if !self.configured || (w, h) != (self.width, self.height) {
            self.width = w;
            self.height = h;
            self.buffers = (0..2).map(|i| make_buffer(&self.shm, qh, w as usize, h as usize, i)).collect();
        }
        self.configured = true;
        self.relayout();
    }

    fn relayout(&mut self) {
        let (cols, rows) = self.grid_size();
        if (cols, rows) != (self.term.cols, self.term.rows) {
            self.term.resize(cols, rows);
            self.pty.resize(cols, rows);
            self.selection = None;
        }
        self.dirty = true;
    }

    fn zoom(&mut self, size: f32) {
        self.fonts.set_size(size);
        self.relayout();
    }

    fn draw(&mut self, qh: &QueueHandle<App>) {
        if !self.configured || !self.dirty || self.frame_pending {
            return;
        }
        let Some(index) = self.buffers.iter().position(|b| !b.busy) else { return };
        self.render(index);
        let buffer = &mut self.buffers[index];
        buffer.busy = true;
        self.surface.attach(Some(&buffer.buffer), 0, 0);
        self.surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
        self.surface.frame(qh, ());
        self.surface.commit();
        self.frame_pending = true;
        self.dirty = false;
    }

    fn render(&mut self, index: usize) {
        let (w, h) = (self.width as usize, self.height as usize);
        let (cw, ch, ascent) = (self.fonts.cell_w, self.fonts.cell_h, self.fonts.ascent);
        let map = &mut self.buffers[index].map;
        let px = unsafe { std::slice::from_raw_parts_mut(map.as_mut_ptr().cast::<u32>(), w * h) };
        px.fill(paint(config::get().colors.background));

        let term = &self.term;
        let (selection, focused) = (self.selection, self.focused);
        let search = self.search.as_ref().map(|s| (s.query.clone(), s.found));
        let (cx, cy) = (term.cur.x, term.cur.y);
        let cursor_visible = term.cursor_visible && term.view == 0;
        let style = term.cursor_style.unwrap_or(config::get().cursor.style);
        let block = matches!(style, CursorStyle::Block);
        let colors = |x: usize, y: usize| {
            let cell = term.cell(x, y);
            if block && cursor_visible && focused && (x, y) == (cx, cy) {
                return (config::get().cursor.text, config::get().cursor.color);
            }
            let invert = (cell.flags & REVERSE != 0) ^ selected(selection, x, y);
            if invert { (cell.bg, cell.fg) } else { (cell.fg, cell.bg) }
        };

        for y in 0..term.rows {
            for x in 0..term.cols {
                let (ox, oy) = (border() + x * cw, border() + y * ch);
                if ox + cw <= w && oy + ch <= h {
                    fill(px, w, ox, oy, cw, ch, paint(colors(x, y).1));
                }
            }
        }

        for y in 0..term.rows {
            for x in 0..term.cols {
                let cell = term.cell(x, y);
                let (ox, oy) = (border() + x * cw, border() + y * ch);
                if ox + cw > w || oy + ch > h {
                    continue;
                }
                let (mut fg, bg) = colors(x, y);
                if cell.flags & DIM != 0 {
                    fg = blend(0xff000000 | bg, fg, 128) & 0xffffff;
                }
                if cell.ch != ' ' {
                    let spill = x + 1 < term.cols && term.cell(x + 1, y).ch == ' ' && ox + 2 * cw <= w;
                    let right = if spill { ox + 2 * cw } else { ox + cw };
                    let glyph = self.fonts.glyph(cell.ch, cell.flags & BOLD != 0);
                    blit(px, w, glyph, (ox, oy), (right, oy + ch), fg);
                }
                if cell.flags & STRIKE != 0 {
                    fill(px, w, ox, oy + ch / 2, cw, 1, 0xff000000 | fg);
                }
                if cell.flags & UNDERLINE != 0 {
                    fill(px, w, ox, (oy + ascent + 1).min(oy + ch - 1), cw, 1, 0xff000000 | fg);
                }
            }
        }

        if cursor_visible && cx < term.cols && cy < term.rows {
            let (ox, oy) = (border() + cx * cw, border() + cy * ch);
            if ox + cw <= w && oy + ch <= h {
                let color = 0xff000000 | config::get().cursor.color;
                let thick = config::get().cursor.thickness.min(cw).min(ch);
                match style {
                    CursorStyle::Beam => fill(px, w, ox, oy, thick, ch, color),
                    CursorStyle::Underline => fill(px, w, ox, oy + ch - thick, cw, thick, color),
                    CursorStyle::Block if !focused => {
                        fill(px, w, ox, oy, cw, 1, color);
                        fill(px, w, ox, oy + ch - 1, cw, 1, color);
                        fill(px, w, ox, oy, 1, ch, color);
                        fill(px, w, ox + cw - 1, oy, 1, ch, color);
                    }
                    CursorStyle::Block => {}
                }
            }
        }

        if let Some((query, found)) = search {
            let (fg, bg) = (config::get().cursor.text, config::get().cursor.color);
            let oy = border() + (term.rows - 1) * ch;
            if oy + ch <= h {
                fill(px, w, border(), oy, (term.cols * cw).min(w - border()), ch, paint(bg));
            }
            let miss = if found || query.is_empty() { "" } else { "  (no match)" };
            let label = format!("search: {query}{miss}");
            for (x, c) in label.chars().take(term.cols).enumerate() {
                let ox = border() + x * cw;
                if c != ' ' && ox + cw <= w && oy + ch <= h {
                    blit(px, w, self.fonts.glyph(c, false), (ox, oy), (ox + cw, oy + ch), fg);
                }
            }
            let caret = border() + ("search: ".len() + query.chars().count()) * cw;
            if caret + 2 <= w && oy + ch <= h {
                fill(px, w, caret, oy, 2, ch, 0xff000000 | fg);
            }
        }
    }

    fn read_pty(&mut self, qh: &QueueHandle<App>) {
        let mut buf = [0u8; 65536];
        let n = self.pty.read(&mut buf);
        if n < 0 {
            if !matches!(std::io::Error::last_os_error().kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) {
                self.exit = true;
            }
            return;
        }
        if n == 0 {
            self.exit = true;
            return;
        }
        self.term.feed(&buf[..n as usize]);
        if self.term.scrolled {
            self.term.scrolled = false;
            self.selection = None;
        }
        if let Some(title) = self.term.title.take() {
            self.toplevel.set_title(title);
        }
        if let Some(text) = self.term.clipboard.take() {
            self.set_clipboard(text, qh);
        }
        if !self.term.reply.is_empty() {
            let reply = std::mem::take(&mut self.term.reply);
            self.pty.send(&reply);
        }
        if self.term.sync {
            self.sync_deadline.get_or_insert(Instant::now() + Duration::from_millis(150));
        } else {
            self.sync_deadline = None;
            self.dirty = true;
        }
    }

    fn cell_at(&self, x: f64, y: f64) -> Pos {
        let col = ((x - border() as f64) / self.fonts.cell_w as f64).floor().max(0.0) as usize;
        let row = ((y - border() as f64) / self.fonts.cell_h as f64).floor().max(0.0) as usize;
        (col.min(self.term.cols - 1), row.min(self.term.rows - 1))
    }

    fn click(&mut self) {
        let now = Instant::now();
        let quick = now.duration_since(self.last_click).as_millis() < config::get().mouse.multi_click_ms as u128;
        self.clicks = if quick { self.clicks % 3 + 1 } else { 1 };
        self.last_click = now;
        let (x, y) = self.cell_at(self.mouse.0, self.mouse.1);
        self.anchor = None;
        match self.clicks {
            1 => {
                self.anchor = Some((x, y));
                self.selection = None;
            }
            2 => {
                let (start, end) = self.term.word_at(x, y);
                self.selection = Some(((start, y), (end, y)));
            }
            _ => self.selection = Some(((0, y), (self.term.cols - 1, y))),
        }
        self.dirty = true;
    }

    fn drag(&mut self) {
        if let Some(anchor) = self.anchor {
            let cell = self.cell_at(self.mouse.0, self.mouse.1);
            if cell != anchor || self.selection.is_some() {
                self.selection = Some((anchor, cell));
                self.dirty = true;
            }
        }
    }

    fn selection_text(&self) -> String {
        let Some((a, b)) = self.selection else { return String::new() };
        let (a, b) = ordered(a, b);
        self.term.text(a, b)
    }

    fn copy(&mut self, qh: &QueueHandle<App>) {
        let text = self.selection_text();
        self.set_clipboard(text, qh);
    }

    fn set_clipboard(&mut self, text: String, qh: &QueueHandle<App>) {
        if text.is_empty() {
            return;
        }
        let source = self.data_manager.create_data_source(qh, ());
        for mime in COPY_MIMES {
            source.offer(mime.to_string());
        }
        self.data_device.set_selection(Some(&source), self.serial);
        if let Some(old) = self.clip_source.replace(source) {
            old.destroy();
        }
        self.clip_text = text;
    }

    fn publish_primary(&mut self, qh: &QueueHandle<App>) {
        let (Some(manager), Some(device)) = (&self.primary_manager, &self.primary_device) else { return };
        let text = self.selection_text();
        if text.is_empty() {
            return;
        }
        let source = manager.create_source(qh, ());
        for mime in COPY_MIMES {
            source.offer(mime.to_string());
        }
        device.set_selection(Some(&source), self.serial);
        if let Some(old) = self.primary_source.replace(source) {
            old.destroy();
        }
        self.primary_text = text;
    }

    fn paste_begin(&mut self) {
        self.snap_to_bottom();
        if self.term.bracketed_paste {
            self.pty.send(b"\x1b[200~");
        }
    }

    fn paste_end(&mut self) {
        if self.term.bracketed_paste {
            self.pty.send(b"\x1b[201~");
        }
    }

    fn paste_chunk(&mut self, data: &[u8]) {
        let data: Vec<u8> = data.iter().map(|&b| if b == b'\n' { b'\r' } else { b }).collect();
        self.pty.send(&data);
    }

    fn paste(&mut self, primary: bool) {
        if self.paste.is_some() {
            return;
        }
        let local = if primary {
            self.primary_source.is_some().then(|| self.primary_text.clone())
        } else {
            self.clip_source.is_some().then(|| self.clip_text.clone())
        };
        if let Some(text) = local {
            self.paste_begin();
            self.paste_chunk(text.as_bytes());
            self.paste_end();
            return;
        }
        let mimes = if primary { &self.primary_mimes } else { &self.offer_mimes };
        let Some(mime) = PASTE_MIMES.iter().find(|m| mimes.iter().any(|o| o == *m)) else { return };
        let mut fds = [0; 2];
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return;
        }
        let (reader, writer) = unsafe { (File::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        unsafe { libc::fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK) };
        match (primary, &self.primary_offer, &self.offer) {
            (true, Some(offer), _) => offer.receive(mime.to_string(), writer.as_fd()),
            (false, _, Some(offer)) => offer.receive(mime.to_string(), writer.as_fd()),
            _ => return,
        }
        self.paste = Some(reader);
        self.paste_begin();
    }

    fn read_paste(&mut self) {
        let Some(file) = &mut self.paste else { return };
        let mut buf = [0u8; 4096];
        match file.read(&mut buf) {
            Ok(n) if n > 0 => self.paste_chunk(&buf[..n]),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
            _ => {
                self.paste = None;
                self.paste_end();
            }
        }
    }

    fn mods(&self) -> u8 {
        let Some(state) = &self.xkb_state else { return 0 };
        let active = |name| state.mod_name_is_active(name, xkb::STATE_MODS_EFFECTIVE);
        active(xkb::MOD_NAME_SHIFT) as u8
            | (active(xkb::MOD_NAME_ALT) as u8) << 1
            | (active(xkb::MOD_NAME_CTRL) as u8) << 2
    }

    fn mouse_mods(&self) -> u8 {
        let mods = self.mods();
        (mods & config::ALT != 0) as u8 * 8 | (mods & config::CTRL != 0) as u8 * 16
    }

    fn grabbed(&self) -> bool {
        self.term.mouse > 0 && self.mods() & config::SHIFT == 0
    }

    fn report(&mut self, code: u8, release: bool) {
        let (x, y) = self.cell_at(self.mouse.0, self.mouse.1);
        let (x, y) = (x + 1, y + 1);
        if self.term.mouse_sgr {
            let end = if release { 'm' } else { 'M' };
            self.pty.send(format!("\x1b[<{code};{x};{y}{end}").as_bytes());
        } else if x <= 223 && y <= 223 {
            let code = if release { code | 3 } else { code };
            self.pty.send(&[0x1b, b'[', b'M', code + 32, x as u8 + 32, y as u8 + 32]);
        }
    }

    fn report_motion(&mut self) {
        let cell = self.cell_at(self.mouse.0, self.mouse.1);
        if cell == self.last_cell {
            return;
        }
        self.last_cell = cell;
        match (self.term.mouse, self.held) {
            (2 | 3, Some(button)) => self.report(button | 32 | self.mouse_mods(), false),
            (3, None) => self.report(3 | 32 | self.mouse_mods(), false),
            _ => {}
        }
    }

    fn scroll(&mut self, value: f64) {
        self.wheel += value;
        let notches = (self.wheel / 15.0) as i32;
        if notches == 0 {
            return;
        }
        self.wheel -= notches as f64 * 15.0;
        let up = notches < 0;
        for _ in 0..notches.abs() {
            if self.grabbed() {
                self.report(64 + !up as u8 | self.mouse_mods(), false);
            } else if self.term.in_alt && self.term.mouse == 0 {
                let prefix = if self.term.app_cursor { "\x1bO" } else { "\x1b[" };
                let key = if up { 'A' } else { 'B' };
                for _ in 0..config::get().mouse.scroll_lines {
                    self.pty.send(format!("{prefix}{key}").as_bytes());
                }
            } else if !self.term.in_alt {
                let lines = config::get().mouse.scroll_lines as isize;
                self.scroll_view(if up { lines } else { -lines });
            }
        }
    }

    /// Scrolls the history view. Returns false when the key should go to the application
    /// instead, which is the case on the alternate screen.
    fn scroll_view(&mut self, delta: isize) -> bool {
        if self.term.in_alt {
            return false;
        }
        if self.term.scroll_view(delta) {
            self.selection = None;
            self.dirty = true;
        }
        true
    }

    fn snap_to_bottom(&mut self) {
        if self.term.reset_view() {
            self.selection = None;
            self.dirty = true;
        }
    }

    fn on_key(&mut self, key: u32, qh: &QueueHandle<App>) {
        let Some(state) = &self.xkb_state else { return };
        let code = xkb::Keycode::new(key + 8);
        let sym = state.key_get_one_sym(code).raw();
        let text = state.key_get_utf8(code);
        let mods = self.mods();

        if self.search.is_some() {
            self.search_key(sym, &text, mods, qh);
            return;
        }
        if let Some(shortcut) = config::get().shortcuts.iter().find(|s| s.mods == mods && s.sym == sym) {
            let page = self.term.rows.saturating_sub(1).max(1) as isize;
            let consumed = match shortcut.action {
                Action::Copy => {
                    self.copy(qh);
                    true
                }
                Action::Paste => {
                    self.paste(false);
                    true
                }
                Action::ZoomIn => {
                    self.zoom(self.fonts.size() + config::get().font.zoom_step);
                    true
                }
                Action::ZoomOut => {
                    self.zoom(self.fonts.size() - config::get().font.zoom_step);
                    true
                }
                Action::ZoomReset => {
                    self.zoom(config::get().font.size);
                    true
                }
                Action::ScrollLineUp => self.scroll_view(1),
                Action::ScrollLineDown => self.scroll_view(-1),
                Action::ScrollPageUp => self.scroll_view(page),
                Action::ScrollPageDown => self.scroll_view(-page),
                Action::ScrollTop => self.scroll_view(isize::MAX),
                Action::ScrollBottom => self.scroll_view(isize::MIN),
                Action::Search => self.start_search(),
                Action::PrevPrompt => self.jump_prompt(true),
                Action::NextPrompt => self.jump_prompt(false),
                Action::NewWindow => {
                    self.new_window();
                    true
                }
            };
            if consumed {
                return;
            }
        }
        let bytes = keys::encode(sym, &text, mods, self.term.app_cursor);
        if !bytes.is_empty() {
            self.snap_to_bottom();
            self.pty.send(&bytes);
        }
    }

    fn jump_prompt(&mut self, back: bool) -> bool {
        if self.term.in_alt {
            return false;
        }
        if self.term.jump_prompt(back) {
            self.selection = None;
            self.dirty = true;
        }
        true
    }

    fn start_search(&mut self) -> bool {
        if self.term.in_alt {
            return false;
        }
        self.search = Some(Search { query: String::new(), pos: (self.term.lines() - 1, usize::MAX), found: false });
        self.selection = None;
        self.dirty = true;
        true
    }

    fn search_run(&mut self, from: Pos, backward: bool, keep: bool) {
        let Some(search) = &mut self.search else { return };
        let hit = if search.query.is_empty() { None } else { self.term.find(&search.query, from, backward) };
        match hit {
            Some((line, start, end)) => {
                search.found = true;
                search.pos = (line, start);
                let row = self.term.reveal(line);
                self.selection = Some(((start, row), (end, row)));
            }
            None if keep => {}
            None => {
                search.found = false;
                self.selection = None;
            }
        }
        self.dirty = true;
    }

    fn search_step(&mut self, backward: bool) {
        let Some(search) = &self.search else { return };
        if !search.found {
            return;
        }
        let (line, col) = search.pos;
        let from = match (backward, col, line) {
            (true, 0, 0) => return,
            (true, 0, _) => (line - 1, usize::MAX),
            (true, _, _) => (line, col - 1),
            (false, ..) => (line, col + 1),
        };
        self.search_run(from, backward, true);
    }

    fn search_key(&mut self, sym: u32, text: &str, mods: u8, qh: &QueueHandle<App>) {
        let Some(search) = &mut self.search else { return };
        let ctrl = mods & config::CTRL != 0;
        match sym {
            KEY_Escape => {
                self.search = None;
                self.selection = None;
                self.dirty = true;
            }
            KEY_Return => {
                self.search = None;
                self.dirty = true;
                self.publish_primary(qh);
            }
            KEY_BackSpace => {
                search.query.pop();
                search.pos = (self.term.lines() - 1, usize::MAX);
                let from = search.pos;
                self.search_run(from, true, false);
            }
            KEY_Up => self.search_step(true),
            KEY_Down => self.search_step(false),
            _ if ctrl && (sym == KEY_r || sym == KEY_R) => self.search_step(true),
            _ if ctrl && (sym == KEY_s || sym == KEY_S) => self.search_step(false),
            _ if !ctrl && mods & config::ALT == 0 && !text.is_empty() && !text.chars().any(char::is_control) => {
                search.query.push_str(text);
                let from = search.pos;
                self.search_run(from, true, false);
            }
            _ => {}
        }
    }

    fn open(&self, url: &str) {
        let mut command = Command::new(&config::get().mouse.url_launcher);
        command.arg(url);
        launch(&mut command);
    }

    fn new_window(&self) {
        let Ok(exe) = std::env::current_exe() else { return };
        let mut command = Command::new(exe);
        if let Some(cwd) = &self.term.cwd {
            command.current_dir(cwd);
        }
        launch(&mut command);
    }

    fn set_keymap(&mut self, fd: OwnedFd, size: u32) {
        let file = File::from(fd);
        let Ok(map) = (unsafe { MmapOptions::new().len(size as usize).map_copy_read_only(&file) }) else { return };
        let Ok(text) = std::str::from_utf8(&map[..size as usize - 1]) else { return };
        let keymap = xkb::Keymap::new_from_string(&self.xkb, text.to_string(), xkb::KEYMAP_FORMAT_TEXT_V1, xkb::KEYMAP_COMPILE_NO_FLAGS);
        if let Some(keymap) = keymap {
            self.xkb_state = Some(xkb::State::new(&keymap));
            self.keymap = Some(keymap);
        }
    }
}

macro_rules! ignore {
    ($($proxy:ty),*) => {$(
        impl Dispatch<$proxy, ()> for App {
            fn event(_: &mut Self, _: &$proxy, _: <$proxy as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}

ignore!(
    WlCompositor,
    WlShm,
    WlShmPool,
    WlSurface,
    WlDataDeviceManager,
    WpCursorShapeManagerV1,
    WpCursorShapeDeviceV1,
    ZwpPrimarySelectionDeviceManagerV1
);

impl Dispatch<WlRegistry, GlobalListContents> for App {
    fn event(_: &mut Self, _: &WlRegistry, _: wayland_client::protocol::wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlBuffer, usize> for App {
    fn event(app: &mut Self, _: &WlBuffer, event: wl_buffer::Event, index: &usize, _: &Connection, _: &QueueHandle<Self>) {
        if let (wl_buffer::Event::Release, Some(buffer)) = (event, app.buffers.get_mut(*index)) {
            buffer.busy = false;
        }
    }
}

impl Dispatch<WlCallback, ()> for App {
    fn event(app: &mut Self, _: &WlCallback, event: wl_callback::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_callback::Event::Done { .. } = event {
            app.frame_pending = false;
        }
    }
}

impl Dispatch<XdgWmBase, ()> for App {
    fn event(_: &mut Self, base: &XdgWmBase, event: xdg_wm_base::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for App {
    fn event(app: &mut Self, surface: &XdgSurface, event: xdg_surface::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            app.apply_size(qh);
        }
    }
}

impl Dispatch<XdgToplevel, ()> for App {
    fn event(app: &mut Self, _: &XdgToplevel, event: xdg_toplevel::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            xdg_toplevel::Event::Configure { width, height, .. } => app.pending = (width as u32, height as u32),
            xdg_toplevel::Event::Close => app.exit = true,
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for App {
    fn event(app: &mut Self, seat: &WlSeat, event: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event else { return };
        if caps.contains(wl_seat::Capability::Keyboard) && app.keyboard.is_none() {
            app.keyboard = Some(seat.get_keyboard(qh, ()));
        }
        if caps.contains(wl_seat::Capability::Pointer) && app.pointer.is_none() {
            let pointer = seat.get_pointer(qh, ());
            app.cursor_shape = app.cursor_manager.as_ref().map(|m| m.get_pointer(&pointer, qh, ()));
            app.pointer = Some(pointer);
        }
    }
}

impl Dispatch<WlKeyboard, ()> for App {
    fn event(app: &mut Self, _: &WlKeyboard, event: wl_keyboard::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        match event {
            wl_keyboard::Event::Keymap { format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1), fd, size } => app.set_keymap(fd, size),
            wl_keyboard::Event::Enter { serial, .. } => {
                app.serial = serial;
                app.focused = true;
                app.dirty = true;
                if app.term.focus_events {
                    app.pty.send(b"\x1b[I");
                }
            }
            wl_keyboard::Event::Leave { .. } => {
                app.focused = false;
                app.repeat = None;
                app.dirty = true;
                if app.term.focus_events {
                    app.pty.send(b"\x1b[O");
                }
            }
            wl_keyboard::Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                if let Some(state) = &mut app.xkb_state {
                    state.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                app.repeat_rate = rate as u64;
                app.repeat_delay = delay as u64;
            }
            wl_keyboard::Event::Key { serial, key, state: WEnum::Value(state), .. } => {
                app.serial = serial;
                match state {
                    wl_keyboard::KeyState::Pressed => {
                        app.on_key(key, qh);
                        let repeats = app.keymap.as_ref().is_some_and(|k| k.key_repeats(xkb::Keycode::new(key + 8)));
                        if repeats && app.repeat_rate > 0 {
                            app.repeat = Some(Repeat { key, next: Instant::now() + Duration::from_millis(app.repeat_delay) });
                        }
                    }
                    wl_keyboard::KeyState::Released => {
                        if app.repeat.is_some_and(|r| r.key == key) {
                            app.repeat = None;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, ()> for App {
    fn event(app: &mut Self, _: &WlPointer, event: wl_pointer::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        match event {
            wl_pointer::Event::Enter { serial, surface_x, surface_y, .. } => {
                app.mouse = (surface_x, surface_y);
                if let Some(shape) = &app.cursor_shape {
                    shape.set_shape(serial, Shape::Text);
                }
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                app.mouse = (surface_x, surface_y);
                if app.grabbed() {
                    app.report_motion();
                } else {
                    app.drag();
                }
            }
            wl_pointer::Event::Axis { axis: WEnum::Value(wl_pointer::Axis::VerticalScroll), value, .. } => app.scroll(value),
            wl_pointer::Event::Button { serial, button, state: WEnum::Value(state), .. } => {
                app.serial = serial;
                if app.grabbed() {
                    let code = match button {
                        BTN_LEFT => 0,
                        BTN_MIDDLE => 1,
                        BTN_RIGHT => 2,
                        _ => return,
                    };
                    let pressed = matches!(state, wl_pointer::ButtonState::Pressed);
                    app.held = pressed.then_some(code);
                    app.last_cell = app.cell_at(app.mouse.0, app.mouse.1);
                    app.report(code | app.mouse_mods(), !pressed);
                    return;
                }
                match (button, state) {
                    (BTN_LEFT, wl_pointer::ButtonState::Pressed) => {
                        let (x, y) = app.cell_at(app.mouse.0, app.mouse.1);
                        let url = (app.mods() & config::CTRL != 0).then(|| app.term.url_at(x, y)).flatten();
                        match url {
                            Some(url) => app.open(&url),
                            None => app.click(),
                        }
                    }
                    (BTN_LEFT, wl_pointer::ButtonState::Released) => {
                        app.anchor = None;
                        if app.selection.is_some() {
                            app.publish_primary(qh);
                        }
                    }
                    (BTN_MIDDLE, wl_pointer::ButtonState::Pressed) => app.paste(true),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlDataDevice, ()> for App {
    fn event(app: &mut Self, _: &WlDataDevice, event: wl_data_device::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_data_device::Event::DataOffer { .. } => app.pending_mimes.clear(),
            wl_data_device::Event::Selection { id } => {
                if let Some(old) = std::mem::replace(&mut app.offer, id) {
                    old.destroy();
                }
                app.offer_mimes = std::mem::take(&mut app.pending_mimes);
            }
            _ => {}
        }
    }

    event_created_child!(App, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataOffer, ()> for App {
    fn event(app: &mut Self, _: &WlDataOffer, event: wl_data_offer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_data_offer::Event::Offer { mime_type } = event {
            app.pending_mimes.push(mime_type);
        }
    }
}

impl Dispatch<WlDataSource, ()> for App {
    fn event(app: &mut Self, source: &WlDataSource, event: wl_data_source::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_data_source::Event::Send { fd, .. } => {
                let _ = File::from(fd).write_all(app.clip_text.as_bytes());
            }
            wl_data_source::Event::Cancelled => {
                source.destroy();
                if app.clip_source.as_ref().is_some_and(|s| s.id() == source.id()) {
                    app.clip_source = None;
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpPrimarySelectionDeviceV1, ()> for App {
    fn event(app: &mut Self, _: &ZwpPrimarySelectionDeviceV1, event: zwp_primary_selection_device_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwp_primary_selection_device_v1::Event::DataOffer { .. } => app.pending_mimes.clear(),
            zwp_primary_selection_device_v1::Event::Selection { id } => {
                if let Some(old) = std::mem::replace(&mut app.primary_offer, id) {
                    old.destroy();
                }
                app.primary_mimes = std::mem::take(&mut app.pending_mimes);
            }
            _ => {}
        }
    }

    event_created_child!(App, ZwpPrimarySelectionDeviceV1, [
        zwp_primary_selection_device_v1::EVT_DATA_OFFER_OPCODE => (ZwpPrimarySelectionOfferV1, ()),
    ]);
}

impl Dispatch<ZwpPrimarySelectionOfferV1, ()> for App {
    fn event(app: &mut Self, _: &ZwpPrimarySelectionOfferV1, event: zwp_primary_selection_offer_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let zwp_primary_selection_offer_v1::Event::Offer { mime_type } = event {
            app.pending_mimes.push(mime_type);
        }
    }
}

impl Dispatch<ZwpPrimarySelectionSourceV1, ()> for App {
    fn event(app: &mut Self, source: &ZwpPrimarySelectionSourceV1, event: zwp_primary_selection_source_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwp_primary_selection_source_v1::Event::Send { fd, .. } => {
                let _ = File::from(fd).write_all(app.primary_text.as_bytes());
            }
            zwp_primary_selection_source_v1::Event::Cancelled => {
                source.destroy();
                if app.primary_source.as_ref().is_some_and(|s| s.id() == source.id()) {
                    app.primary_source = None;
                }
            }
            _ => {}
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let config_error = config::load();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.iter().position(|a| a == "-e").map_or(Vec::new(), |i| args[i + 1..].to_vec());

    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<App>(&conn)?;
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 4..=6, ())?;
    let shm: WlShm = globals.bind(&qh, 1..=1, ())?;
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ())?;
    let seat: WlSeat = globals.bind(&qh, 4..=7, ())?;
    let data_manager: WlDataDeviceManager = globals.bind(&qh, 1..=3, ())?;
    let cursor_manager = globals.bind::<WpCursorShapeManagerV1, _, _>(&qh, 1..=1, ()).ok();
    let data_device = data_manager.get_data_device(&seat, &qh, ());
    let primary_manager = globals.bind::<ZwpPrimarySelectionDeviceManagerV1, _, _>(&qh, 1..=1, ()).ok();
    let primary_device = primary_manager.as_ref().map(|m| m.get_device(&seat, &qh, ()));

    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title(config::get().window.title.clone());
    toplevel.set_app_id(config::get().window.app_id.clone());
    surface.commit();

    let (cols, rows) = (config::get().window.cols, config::get().window.rows);
    let mut term = Term::new(cols, rows);
    if let Some(error) = &config_error {
        eprintln!("softty: config error: {error}");
        let text = format!(
            "\x1b[1;31msoftty: config error\x1b[0m\n{error}\n\x1b[33mUsing default settings.\x1b[0m\n\n"
        );
        term.feed(text.replace('\n', "\r\n").as_bytes());
    }
    let mut app = App {
        shm,
        surface,
        toplevel,
        data_manager,
        data_device,
        cursor_manager,
        cursor_shape: None,
        primary_manager,
        primary_device,
        keyboard: None,
        pointer: None,
        xkb: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
        keymap: None,
        xkb_state: None,
        repeat: None,
        repeat_rate: 0,
        repeat_delay: 0,
        serial: 0,
        term,
        pty: Pty::spawn(&command, cols, rows),
        fonts: Fonts::new(),
        width: 0,
        height: 0,
        pending: (0, 0),
        configured: false,
        buffers: Vec::new(),
        dirty: false,
        frame_pending: false,
        focused: false,
        mouse: (0.0, 0.0),
        anchor: None,
        selection: None,
        last_click: Instant::now(),
        clicks: 0,
        held: None,
        last_cell: (0, 0),
        wheel: 0.0,
        clip_text: String::new(),
        clip_source: None,
        offer: None,
        offer_mimes: Vec::new(),
        pending_mimes: Vec::new(),
        paste: None,
        primary_text: String::new(),
        primary_source: None,
        primary_offer: None,
        primary_mimes: Vec::new(),
        search: None,
        sync_deadline: None,
        exit: false,
    };

    loop {
        queue.dispatch_pending(&mut app)?;
        app.draw(&qh);
        if app.exit {
            return Ok(());
        }
        let _ = queue.flush();
        let Some(guard) = queue.prepare_read() else { continue };

        let mut fds = vec![
            libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd {
                fd: app.pty.fd,
                events: libc::POLLIN | if app.pty.out.is_empty() { 0 } else { libc::POLLOUT },
                revents: 0,
            },
        ];
        if let Some(file) = &app.paste {
            fds.push(libc::pollfd { fd: file.as_raw_fd(), events: libc::POLLIN, revents: 0 });
        }
        let now = Instant::now();
        let timeout = [app.repeat.map(|r| r.next), app.sync_deadline]
            .into_iter()
            .flatten()
            .map(|deadline| deadline.saturating_duration_since(now).as_millis() as i32 + 1)
            .min()
            .unwrap_or(-1);
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };

        if fds[0].revents & libc::POLLIN != 0 {
            match guard.read() {
                Err(WaylandError::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
                Ok(_) => {}
            }
        } else {
            drop(guard);
        }
        if fds[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            app.read_pty(&qh);
        }
        if fds[1].revents & libc::POLLOUT != 0 {
            app.pty.flush();
        }
        if fds.len() > 2 && fds[2].revents != 0 {
            app.read_paste();
        }
        if app.sync_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            app.term.sync = false;
            app.sync_deadline = None;
            app.dirty = true;
        }
        if let Some(repeat) = app.repeat {
            if Instant::now() >= repeat.next {
                app.repeat = Some(Repeat { key: repeat.key, next: repeat.next + Duration::from_millis(1000 / app.repeat_rate.max(1)) });
                app.on_key(repeat.key, &qh);
            }
        }
    }
}
