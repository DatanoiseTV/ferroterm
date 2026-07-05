//! PTY session for the native app: spawns the user's shell and streams its
//! output to the winit event loop as `UserEvent`s (so the loop wakes on data
//! without polling). Adapted from the Tauri app's PTY handling, minus Tauri.

use std::io::{Read, Write};

use portable_pty::{CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use winit::event_loop::EventLoopProxy;

use crate::UserEvent;

pub struct Pty {
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl Pty {
    /// Spawn a shell in a `cols`x`rows` PTY; its output is delivered to the
    /// event loop via `proxy`.
    pub fn spawn(cols: u16, rows: u16, proxy: EventLoopProxy<UserEvent>) -> std::io::Result<Pty> {
        let sys = NativePtySystem::default();
        let pair = sys
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(to_io)?;

        let mut cmd = CommandBuilder::new(default_shell());
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            cmd.cwd(home);
        }

        let child = pair.slave.spawn_command(cmd).map_err(to_io)?;
        drop(pair.slave); // let the child own the slave so EOF is seen on exit

        let mut reader = pair.master.try_clone_reader().map_err(to_io)?;
        let writer = pair.master.take_writer().map_err(to_io)?;

        std::thread::spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if proxy
                            .send_event(UserEvent::PtyData(buf[..n].to_vec()))
                            .is_err()
                        {
                            break; // event loop gone
                        }
                    }
                }
            }
            let _ = proxy.send_event(UserEvent::PtyExit);
        });

        Ok(Pty {
            writer,
            master: pair.master,
            child,
        })
    }

    pub fn write(&mut self, data: &[u8]) {
        let _ = self.writer.write_all(data);
        let _ = self.writer.flush();
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }
}

fn default_shell() -> String {
    if cfg!(windows) {
        std::env::var("COMSPEC").unwrap_or_else(|_| "powershell.exe".into())
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into())
    }
}

fn to_io<E: std::fmt::Display>(e: E) -> std::io::Error {
    std::io::Error::other(e.to_string())
}
