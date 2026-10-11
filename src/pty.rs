use std::ffi::CString;
use std::os::fd::RawFd;

use crate::config;

pub struct Pty {
    pub fd: RawFd,
    pub out: Vec<u8>,
}

fn winsize(cols: usize, rows: usize) -> libc::winsize {
    libc::winsize { ws_row: rows as u16, ws_col: cols as u16, ws_xpixel: 0, ws_ypixel: 0 }
}

impl Pty {
    pub fn spawn(cmd: &[String], cols: usize, rows: usize) -> Pty {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let args = if cmd.is_empty() { vec![shell] } else { cmd.to_vec() };
        let argv: Vec<CString> = args.into_iter().map(|a| CString::new(a).unwrap()).collect();
        let mut ptrs: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        let term = CString::new(config::get().term.as_str()).unwrap();
        let ws = winsize(cols, rows);
        let mut fd = 0;

        let pid = unsafe { libc::forkpty(&mut fd, std::ptr::null_mut(), std::ptr::null(), &ws) };
        if pid < 0 {
            panic!("forkpty: {}", std::io::Error::last_os_error());
        }
        if pid == 0 {
            unsafe {
                libc::signal(libc::SIGPIPE, libc::SIG_DFL);
                libc::setenv(b"TERM\0".as_ptr().cast(), term.as_ptr(), 1);
                libc::setenv(b"COLORTERM\0".as_ptr().cast(), b"truecolor\0".as_ptr().cast(), 1);
                libc::execvp(ptrs[0], ptrs.as_ptr());
                libc::_exit(1);
            }
        }
        unsafe {
            let mut attrs: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut attrs) == 0 {
                attrs.c_iflag |= libc::IUTF8;
                libc::tcsetattr(fd, libc::TCSANOW, &attrs);
            }
            libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK);
        }
        Pty { fd, out: Vec::new() }
    }

    pub fn resize(&self, cols: usize, rows: usize) {
        let ws = winsize(cols, rows);
        unsafe { libc::ioctl(self.fd, libc::TIOCSWINSZ, &ws) };
    }

    pub fn read(&self, buf: &mut [u8]) -> isize {
        unsafe { libc::read(self.fd, buf.as_mut_ptr().cast(), buf.len()) }
    }

    pub fn send(&mut self, data: &[u8]) {
        self.out.extend_from_slice(data);
        self.flush();
    }

    pub fn flush(&mut self) {
        while !self.out.is_empty() {
            let n = unsafe { libc::write(self.fd, self.out.as_ptr().cast(), self.out.len()) };
            if n <= 0 {
                break;
            }
            self.out.drain(..n as usize);
        }
    }
}
