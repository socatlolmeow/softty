use ab_glyph::{point, Font, FontRef, PxScale, ScaleFont};
use memmap2::Mmap;
use std::collections::HashMap;
use std::fs::File;
use std::process::Command;

use crate::config;

#[derive(Default)]
pub struct Bitmap {
    pub x: i32,
    pub y: i32,
    pub width: usize,
    pub height: usize,
    pub alpha: Vec<u8>,
}

pub struct Fonts {
    faces: Vec<FontRef<'static>>,
    paths: HashMap<String, usize>,
    bold: Option<usize>,
    chosen: HashMap<(char, bool), usize>,
    cache: HashMap<(char, bool), Bitmap>,
    size: f32,
    pub cell_w: usize,
    pub cell_h: usize,
    pub ascent: usize,
}

fn match_file(pattern: &str) -> Option<String> {
    let output = Command::new("fc-match").args(["-f", "%{file}", pattern]).output().ok()?;
    let path = String::from_utf8(output.stdout).ok()?;
    (!path.is_empty()).then_some(path)
}

fn load(path: &str) -> Option<FontRef<'static>> {
    let file = File::open(path).ok()?;
    let map: &'static Mmap = Box::leak(Box::new(unsafe { Mmap::map(&file) }.ok()?));
    FontRef::try_from_slice(map).ok()
}

fn px_scale(font: &FontRef, size: f32) -> PxScale {
    font.pt_to_px_scale(size * 0.75).expect("font has no units per em")
}

fn rasterize(font: &FontRef, ch: char, size: f32, baseline: f32) -> Bitmap {
    let glyph = font.glyph_id(ch).with_scale_and_position(px_scale(font, size), point(0.0, baseline));
    let Some(outline) = font.outline_glyph(glyph) else { return Bitmap::default() };
    let bounds = outline.px_bounds();
    let (width, height) = (bounds.width() as usize, bounds.height() as usize);
    let mut alpha = vec![0u8; width * height];
    outline.draw(|x, y, coverage| alpha[y as usize * width + x as usize] = (coverage.min(1.0) * 255.0) as u8);
    Bitmap { x: bounds.min.x as i32, y: bounds.min.y as i32, width, height, alpha }
}

impl Fonts {
    pub fn new() -> Fonts {
        let path = match_file(&config::get().font.family).expect("fc-match found no font");
        let regular = load(&path).unwrap_or_else(|| panic!("cannot load font {path:?}"));
        let mut fonts = Fonts {
            faces: vec![regular],
            paths: HashMap::from([(path, 0)]),
            bold: None,
            chosen: HashMap::new(),
            cache: HashMap::new(),
            size: config::get().font.size,
            cell_w: 0,
            cell_h: 0,
            ascent: 0,
        };
        fonts.measure();
        fonts
    }

    pub fn size(&self) -> f32 {
        self.size
    }

    pub fn set_size(&mut self, size: f32) {
        self.size = size.clamp(6.0, 96.0);
        self.measure();
    }

    fn measure(&mut self) {
        self.cache.clear();
        let regular = &self.faces[0];
        let scaled = regular.as_scaled(px_scale(regular, self.size));
        self.ascent = scaled.ascent().round() as usize;
        self.cell_h = (scaled.ascent() - scaled.descent() + scaled.line_gap()).ceil() as usize;
        self.cell_w = (scaled.h_advance(regular.glyph_id('M')).round() as usize).max(1);
    }

    fn face(&mut self, pattern: &str) -> Option<usize> {
        let path = match_file(pattern)?;
        if let Some(&index) = self.paths.get(&path) {
            return Some(index);
        }
        let font = load(&path)?;
        self.faces.push(font);
        self.paths.insert(path, self.faces.len() - 1);
        Some(self.faces.len() - 1)
    }

    fn pick(&mut self, ch: char, bold: bool) -> usize {
        let primary = if !bold {
            0
        } else if let Some(index) = self.bold {
            index
        } else {
            let index = self.face(&format!("{}:bold", config::get().font.family)).unwrap_or(0);
            self.bold = Some(index);
            index
        };
        if self.faces[primary].glyph_id(ch).0 != 0 {
            return primary;
        }
        let style = if bold { ":bold" } else { "" };
        let pattern = format!("{}{style}:charset={:x}", config::get().font.family, ch as u32);
        match self.face(&pattern) {
            Some(index) if self.faces[index].glyph_id(ch).0 != 0 => index,
            _ => primary,
        }
    }

    pub fn glyph(&mut self, ch: char, bold: bool) -> &Bitmap {
        if !self.cache.contains_key(&(ch, bold)) {
            let face = match self.chosen.get(&(ch, bold)) {
                Some(&face) => face,
                None => {
                    let face = self.pick(ch, bold);
                    self.chosen.insert((ch, bold), face);
                    face
                }
            };
            let bitmap = rasterize(&self.faces[face], ch, self.size, self.ascent as f32);
            self.cache.insert((ch, bold), bitmap);
        }
        &self.cache[&(ch, bold)]
    }
}
