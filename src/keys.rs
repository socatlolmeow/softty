#![allow(non_upper_case_globals)]

use xkbcommon::xkb::keysyms::*;

use crate::config::{ALT, CTRL};

pub fn encode(sym: u32, text: &str, mods: u8, app_cursor: bool) -> Vec<u8> {
    let with_mods = |prefix: &str, code: &str, suffix: char| -> Vec<u8> {
        if mods == 0 {
            format!("{prefix}{code}{suffix}").into_bytes()
        } else {
            format!("\x1b[{};{}{}", if code.is_empty() { "1" } else { code }, mods + 1, suffix).into_bytes()
        }
    };
    let cursor = |letter: char| {
        if app_cursor { with_mods("\x1bO", "", letter) } else { with_mods("\x1b[", "", letter) }
    };
    let tilde = |code: &str| with_mods("\x1b[", code, '~');
    let ss3 = |letter: char| with_mods("\x1bO", "", letter);

    match sym {
        KEY_Up => cursor('A'),
        KEY_Down => cursor('B'),
        KEY_Right => cursor('C'),
        KEY_Left => cursor('D'),
        KEY_Home => cursor('H'),
        KEY_End => cursor('F'),
        KEY_Insert => tilde("2"),
        KEY_Delete => tilde("3"),
        KEY_Prior => tilde("5"),
        KEY_Next => tilde("6"),
        KEY_F1 => ss3('P'),
        KEY_F2 => ss3('Q'),
        KEY_F3 => ss3('R'),
        KEY_F4 => ss3('S'),
        KEY_F5 => tilde("15"),
        KEY_F6 => tilde("17"),
        KEY_F7 => tilde("18"),
        KEY_F8 => tilde("19"),
        KEY_F9 => tilde("20"),
        KEY_F10 => tilde("21"),
        KEY_F11 => tilde("23"),
        KEY_F12 => tilde("24"),
        KEY_ISO_Left_Tab => b"\x1b[Z".to_vec(),
        KEY_BackSpace => {
            let byte = if mods & CTRL != 0 { 0x08 } else { 0x7f };
            if mods & ALT != 0 { vec![0x1b, byte] } else { vec![byte] }
        }
        _ if text.is_empty() => Vec::new(),
        _ if mods & ALT != 0 => [b"\x1b".as_slice(), text.as_bytes()].concat(),
        _ => text.as_bytes().to_vec(),
    }
}
