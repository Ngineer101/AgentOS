//! Terminals: your login shell in a pseudo-terminal the host owns, outside
//! any sandbox (see `agenthost_proto::terminal` for the protocol).
//!
//! Each terminal has three small threads: one reads the shell's output into
//! a ring of the last [`TERMINAL_REPLAY_BYTES`], one writes input (so a
//! shell that isn't reading can't stall the API), one waits for the shell to
//! end. Attached connections pull from the ring at their own pace; the
//! reader stops reading the shell while an attached client is a full window
//! behind, so a flood of output waits for the screen as it would in any
//! terminal. A client that stops acknowledging for [`STALL`] no longer holds
//! the shell up; it skips ahead when it comes back.

use crate::bus::{Bus, BusMsg};
use crate::ctx::Ctx;
use crate::util::{new_id, now_ms};
use agenthost_proto::jsonrpc::RpcError;
use agenthost_proto::*;
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc as std_mpsc, Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// How long a client may sit a full window behind before the shell stops
/// waiting for it.
const STALL: Duration = Duration::from_secs(2);
/// Input waiting for a shell that isn't reading; more is refused.
const MAX_QUEUED_INPUT: usize = 4 * 1024 * 1024;
/// How long a closed terminal's shell gets to end after the hang-up.
const KILL_AFTER: Duration = Duration::from_secs(2);

pub struct Terminals {
    ctx: Arc<Ctx>,
    list: Mutex<Vec<Arc<Term>>>,
}

/// One terminal: the shell, its pseudo-terminal and its kept output.
pub struct Term {
    id: String,
    cwd: String,
    shell: String,
    created_at: i64,
    pid: i32,
    master: File,
    input: Mutex<std_mpsc::Sender<Vec<u8>>>,
    queued: Arc<AtomicUsize>,
    shared: Mutex<Shared>,
    /// Wakes the reader thread: an acknowledgement, a detach or a close.
    cond: Condvar,
    /// Wakes attached connections: more output, or room in their window.
    wake: watch::Sender<u64>,
}

struct Shared {
    buf: VecDeque<u8>,
    /// Offset of `buf`'s first byte in the terminal's whole output.
    start: u64,
    end: u64,
    size: (u16, u16),
    exited: Option<TerminalExit>,
    closed: bool,
    readers: HashMap<u64, Reader>,
    next_reader: u64,
}

/// One attached connection's place in the output.
struct Reader {
    acked: u64,
    /// Where the output stood when it attached: before this is a replay.
    replay_end: u64,
    /// Since when it has been a full window behind.
    behind_since: Option<Instant>,
    /// It stayed behind too long: the shell no longer waits for it.
    lagging: bool,
}

/// What an attached connection should do next.
pub enum Pull {
    /// Send these bytes, which begin at this offset; `true` for a replay
    /// of what was printed before the attach.
    Data(u64, Vec<u8>, bool),
    /// Nothing to send until the terminal wakes it.
    Wait,
    /// The attachment is gone (detached, or the terminal closed).
    Gone,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Term {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn describe(&self) -> Terminal {
        let g = lock(&self.shared);
        Terminal {
            terminal_id: self.id.clone(),
            cwd: self.cwd.clone(),
            shell: self.shell.clone(),
            cols: g.size.0,
            rows: g.size.1,
            created_at: self.created_at,
            exited: g.exited,
        }
    }

    /// Wakes when there may be something new to [`Term::pull`].
    pub fn watch(&self) -> watch::Receiver<u64> {
        self.wake.subscribe()
    }

    fn nudge(&self) {
        self.wake.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Register a reader at the start of the kept output. Returns its id,
    /// where its output begins and where the output stands now.
    pub fn attach(&self) -> (u64, u64, u64) {
        let (id, start, end) = {
            let mut g = lock(&self.shared);
            let id = g.next_reader;
            g.next_reader += 1;
            // Begin after a line break when the start of the output is gone,
            // so the replay doesn't open in the middle of an escape sequence.
            let mut start = g.start;
            if g.start > 0 {
                if let Some(i) = g.buf.iter().take(4096).position(|b| *b == b'\n') {
                    start += i as u64 + 1;
                }
            }
            let end = g.end;
            g.readers.insert(id, Reader { acked: start, replay_end: end, behind_since: None, lagging: false });
            (id, start, g.end)
        };
        // Full-screen programs repaint on a size change; ask for that, since
        // the replay may not hold everything they drew.
        self.signal_foreground(libc::SIGWINCH);
        (id, start, end)
    }

    pub fn detach(&self, reader: u64) {
        lock(&self.shared).readers.remove(&reader);
        self.cond.notify_all();
        self.nudge();
    }

    pub fn ack(&self, reader: u64, offset: u64) {
        let mut g = lock(&self.shared);
        let end = g.end;
        if let Some(r) = g.readers.get_mut(&reader) {
            r.acked = r.acked.max(offset.min(end));
            r.behind_since = None;
            r.lagging = false;
        }
        drop(g);
        self.cond.notify_all();
        self.nudge();
    }

    /// The next piece of output for a reader that has sent up to `pos`.
    pub fn pull(&self, reader: u64, pos: &mut u64) -> Pull {
        let mut g = lock(&self.shared);
        let (start, end) = (g.start, g.end);
        let Some(r) = g.readers.get_mut(&reader) else { return Pull::Gone };
        if *pos < start {
            // It fell further behind than the host keeps: skip ahead.
            *pos = start;
            r.acked = r.acked.max(start);
        }
        let limit = r.acked + TERMINAL_WINDOW_BYTES;
        if *pos >= end || *pos >= limit {
            return Pull::Wait;
        }
        let mut n = (end - *pos).min(limit - *pos).min(TERMINAL_CHUNK_BYTES as u64);
        // A piece is all replay or all new, never both.
        let replay = *pos < r.replay_end;
        if replay {
            n = n.min(r.replay_end - *pos);
        }
        let (n, at) = (n as usize, (*pos - start) as usize);
        let data: Vec<u8> = g.buf.range(at..at + n).copied().collect();
        let offset = *pos;
        *pos += n as u64;
        Pull::Data(offset, data, replay)
    }

    fn write(&self, data: Vec<u8>) -> Result<(), RpcError> {
        if lock(&self.shared).exited.is_some() {
            return Err(RpcError::invalid_state("this terminal's shell has ended"));
        }
        if self.queued.load(Ordering::Relaxed) + data.len() > MAX_QUEUED_INPUT {
            return Err(RpcError::invalid_state("the terminal isn't reading its input"));
        }
        self.queued.fetch_add(data.len(), Ordering::Relaxed);
        lock(&self.input).send(data).map_err(|_| RpcError::invalid_state("this terminal's shell has ended"))
    }

    fn resize(&self, cols: u16, rows: u16) -> bool {
        let mut g = lock(&self.shared);
        if g.size == (cols, rows) {
            return false;
        }
        g.size = (cols, rows);
        set_size(self.master.as_raw_fd(), cols, rows);
        true
    }

    /// Signal whatever is in the foreground of the terminal.
    fn signal_foreground(&self, sig: libc::c_int) {
        // SAFETY: plain libc calls on a descriptor this struct owns.
        unsafe {
            let pgrp = libc::tcgetpgrp(self.master.as_raw_fd());
            if pgrp > 1 {
                libc::kill(-pgrp, sig);
            }
        }
    }

    /// Hang up: the shell and its jobs end as when a terminal window closes.
    fn hang_up(self: &Arc<Self>) {
        {
            let mut g = lock(&self.shared);
            g.closed = true;
            g.readers.clear();
        }
        self.cond.notify_all();
        self.nudge();
        if lock(&self.shared).exited.is_some() {
            return;
        }
        self.signal_foreground(libc::SIGHUP);
        // SAFETY: the shell leads its own session and process group (`pid`),
        // and is not reaped until its waiter thread sets `exited`.
        unsafe { libc::kill(-self.pid, libc::SIGHUP) };
        let me = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(KILL_AFTER);
            if lock(&me.shared).exited.is_none() {
                me.signal_foreground(libc::SIGKILL);
                // SAFETY: as above; `exited` is still unset, so the pid is ours.
                unsafe { libc::kill(-me.pid, libc::SIGKILL) };
            }
        });
    }
}

fn set_size(fd: i32, cols: u16, rows: u16) {
    let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: TIOCSWINSZ reads a winsize.
    unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) };
}

fn clamp_size(cols: Option<u16>, rows: Option<u16>) -> (u16, u16) {
    (cols.unwrap_or(80).clamp(2, TERMINAL_MAX_COLS), rows.unwrap_or(24).clamp(1, TERMINAL_MAX_ROWS))
}

/// The shell to run: `$AGENTHOST_TERMINAL_SHELL` if set, else `$SHELL`, else
/// the password database's, else `/bin/sh`.
fn user_shell() -> String {
    let usable = |s: &String| Path::new(s).is_absolute() && Path::new(s).is_file();
    if let Some(s) = std::env::var("AGENTHOST_TERMINAL_SHELL").ok().filter(usable) {
        return s;
    }
    if let Some(s) = std::env::var("SHELL").ok().filter(usable) {
        return s;
    }
    // SAFETY: getpwuid's result is read before any other call could replace it.
    let pw = unsafe {
        let pw = libc::getpwuid(libc::getuid());
        (!pw.is_null() && !(*pw).pw_shell.is_null()).then(|| std::ffi::CStr::from_ptr((*pw).pw_shell).to_string_lossy().into_owned())
    };
    pw.filter(usable).unwrap_or_else(|| "/bin/sh".into())
}

/// Open a pseudo-terminal: (master, slave).
fn open_pty(cols: u16, rows: u16) -> std::io::Result<(OwnedFd, OwnedFd)> {
    let (mut master, mut slave) = (-1, -1);
    let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: openpty fills the two descriptors, which are owned from here on.
    unsafe {
        if libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &ws as *const _ as *mut _) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let (master, slave) = (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave));
        for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        // Erase whole characters, not bytes, when a line is edited in the
        // kernel (Linux leaves this off).
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(master.as_raw_fd(), &mut t) == 0 {
            t.c_iflag |= libc::IUTF8;
            libc::tcsetattr(master.as_raw_fd(), libc::TCSANOW, &t);
        }
        Ok((master, slave))
    }
}

impl Terminals {
    pub fn new(ctx: Arc<Ctx>) -> Arc<Self> {
        Arc::new(Self { ctx, list: Mutex::new(vec![]) })
    }

    pub fn list(&self) -> Vec<Terminal> {
        lock(&self.list).iter().map(|t| t.describe()).collect()
    }

    pub fn get(&self, id: &str) -> Result<Arc<Term>, RpcError> {
        lock(&self.list).iter().find(|t| t.id == id).cloned().ok_or_else(|| RpcError::not_found("no such terminal"))
    }

    pub fn open(&self, p: TerminalOpenParams) -> Result<Terminal, RpcError> {
        let (shell, home) = (user_shell(), self.ctx.cfg.home_dir.clone());
        let cwd = match &p.cwd {
            Some(c) => {
                let c = PathBuf::from(c);
                if !c.is_absolute() || !c.is_dir() {
                    return Err(RpcError::invalid_params(format!("{} is not a folder", c.display())));
                }
                c
            }
            None => home.clone(),
        };
        if lock(&self.list).len() >= MAX_TERMINALS {
            return Err(RpcError::invalid_state(format!("{MAX_TERMINALS} terminals are open; close one first")));
        }
        let (cols, rows) = clamp_size(p.cols, p.rows);
        let (master, slave) = open_pty(cols, rows).map_err(|e| RpcError::internal(format!("could not open a terminal: {e}")))?;
        let stdio = || slave.try_clone().map(Stdio::from);
        let io = |e: std::io::Error| RpcError::internal(format!("could not open a terminal: {e}"));
        // A login shell ("-zsh"), so it reads your profile: the host's own
        // environment is a service's, without your PATH.
        let name = Path::new(&shell).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "sh".into());
        let mut cmd = Command::new(&shell);
        cmd.arg0(format!("-{name}"))
            .current_dir(&cwd)
            .stdin(stdio().map_err(io)?)
            .stdout(stdio().map_err(io)?)
            .stderr(stdio().map_err(io)?)
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("TERM_PROGRAM", "AgentOS")
            .env("SHELL", &shell)
            .env("HOME", &home)
            .env("PWD", &cwd);
        // The host's own settings are not the shell's business.
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("AGENTHOST_") {
                cmd.env_remove(k);
            }
        }
        if ["LC_ALL", "LC_CTYPE", "LANG"].iter().all(|k| std::env::var_os(k).is_none()) {
            cmd.env("LANG", "en_US.UTF-8");
        }
        // SAFETY: only async-signal-safe calls between fork and exec. Standard
        // input is already the terminal here: make it this new session's.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn().map_err(|e| RpcError::internal(format!("could not start {shell}: {e}")))?;
        drop(slave);
        drop(cmd);
        let master = File::from(master);
        let (mut out, mut inp) = (master.try_clone().map_err(io)?, master.try_clone().map_err(io)?);
        let (in_tx, in_rx) = std_mpsc::channel::<Vec<u8>>();
        let (wake, _) = watch::channel(0u64);
        let queued = Arc::new(AtomicUsize::new(0));
        let term = Arc::new(Term {
            id: new_id("term"),
            cwd: cwd.display().to_string(),
            shell: shell.clone(),
            created_at: now_ms(),
            pid: child.id() as i32,
            master,
            input: Mutex::new(in_tx),
            queued: queued.clone(),
            shared: Mutex::new(Shared {
                buf: VecDeque::new(),
                start: 0,
                end: 0,
                size: (cols, rows),
                exited: None,
                closed: false,
                readers: HashMap::new(),
                next_reader: 1,
            }),
            cond: Condvar::new(),
            wake,
        });

        // Output: the shell into the ring.
        let t = term.clone();
        std::thread::Builder::new()
            .name("terminal-read".into())
            .spawn(move || {
                let mut chunk = vec![0u8; 64 * 1024];
                loop {
                    if !t.wait_for_room() {
                        break;
                    }
                    match out.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => t.push(&chunk[..n]),
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        // EIO: nothing holds the other side open any more.
                        Err(_) => break,
                    }
                }
            })
            .map_err(io)?;

        // Input: queued, so a shell that isn't reading stalls only this thread.
        std::thread::Builder::new()
            .name("terminal-write".into())
            .spawn(move || {
                while let Ok(data) = in_rx.recv() {
                    let ok = inp.write_all(&data).is_ok();
                    queued.fetch_sub(data.len(), Ordering::Relaxed);
                    if !ok {
                        break;
                    }
                }
            })
            .map_err(io)?;

        // The shell's end.
        let t = term.clone();
        let bus = self.ctx.bus.clone();
        std::thread::Builder::new()
            .name("terminal-wait".into())
            .spawn(move || {
                use std::os::unix::process::ExitStatusExt;
                let exit = match child.wait() {
                    Ok(st) => TerminalExit { code: st.code(), signal: st.signal() },
                    Err(_) => TerminalExit { code: None, signal: None },
                };
                // Let the reader take in the shell's last words first.
                std::thread::sleep(Duration::from_millis(30));
                let closed = {
                    let mut g = lock(&t.shared);
                    g.exited = Some(exit);
                    g.closed
                };
                t.nudge();
                if !closed {
                    publish(&bus, &t, false);
                }
            })
            .map_err(io)?;

        let described = term.describe();
        lock(&self.list).push(term.clone());
        self.ctx.audit.log("terminal.open", json!({"terminal_id": term.id, "cwd": term.cwd, "shell": term.shell, "pid": term.pid}));
        publish(&self.ctx.bus, &term, false);
        Ok(described)
    }

    pub fn input(&self, p: TerminalInputParams) -> Result<(), RpcError> {
        use base64::Engine as _;
        let data = base64::engine::general_purpose::STANDARD.decode(p.data.as_bytes()).map_err(|_| RpcError::invalid_params("data is not base64"))?;
        if data.len() > TERMINAL_INPUT_BYTES {
            return Err(RpcError::invalid_params(format!("more than {TERMINAL_INPUT_BYTES} bytes of input at once")));
        }
        self.get(&p.terminal_id)?.write(data)
    }

    pub fn resize(&self, p: TerminalResizeParams) -> Result<Terminal, RpcError> {
        let t = self.get(&p.terminal_id)?;
        let (cols, rows) = clamp_size(Some(p.cols), Some(p.rows));
        if t.resize(cols, rows) {
            publish(&self.ctx.bus, &t, false);
        }
        Ok(t.describe())
    }

    pub fn close(&self, id: &str) -> Result<(), RpcError> {
        let t = {
            let mut list = lock(&self.list);
            let i = list.iter().position(|t| t.id == id).ok_or_else(|| RpcError::not_found("no such terminal"))?;
            list.remove(i)
        };
        t.hang_up();
        self.ctx.audit.log("terminal.close", json!({"terminal_id": t.id}));
        publish(&self.ctx.bus, &t, true);
        Ok(())
    }

    /// End every terminal (the host is stopping).
    pub fn close_all(&self) {
        for t in std::mem::take(&mut *lock(&self.list)) {
            t.hang_up();
        }
    }
}

fn publish(bus: &Bus, t: &Term, removed: bool) {
    bus.publish(BusMsg::TerminalChanged(TerminalChanged { terminal: t.describe(), removed }));
}

impl Term {
    /// Reader thread: wait while an attached client is a full window
    /// behind. False once the terminal is closed.
    fn wait_for_room(&self) -> bool {
        let mut g = lock(&self.shared);
        loop {
            if g.closed {
                return false;
            }
            let (end, now) = (g.end, Instant::now());
            let mut wait: Option<Duration> = None;
            for r in g.readers.values_mut() {
                if r.lagging || end - r.acked < TERMINAL_WINDOW_BYTES {
                    r.behind_since = None;
                    continue;
                }
                let since = *r.behind_since.get_or_insert(now);
                let waited = now.duration_since(since);
                if waited >= STALL {
                    r.lagging = true;
                } else {
                    wait = Some(wait.map_or(STALL - waited, |w| w.min(STALL - waited)));
                }
            }
            match wait {
                None => return true,
                Some(d) => g = self.cond.wait_timeout(g, d).unwrap_or_else(|e| e.into_inner()).0,
            }
        }
    }

    fn push(&self, data: &[u8]) {
        let mut g = lock(&self.shared);
        g.buf.extend(data);
        g.end += data.len() as u64;
        let over = g.buf.len().saturating_sub(TERMINAL_REPLAY_BYTES);
        if over > 0 {
            g.buf.drain(..over);
            g.start += over as u64;
        }
        drop(g);
        self.nudge();
    }
}

/// One connection's attachment to a terminal: forwards its output until
/// dropped.
pub struct Attachment {
    term: Arc<Term>,
    reader: u64,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Attachment {
    pub fn new(term: Arc<Term>, reader: u64) -> Self {
        Self { term, reader, task: None }
    }

    pub fn ack(&self, offset: u64) {
        self.term.ack(self.reader, offset);
    }

    /// Start sending the terminal's output, from `offset`, with `send`.
    /// `send` answers false once the connection is gone.
    pub fn start<F, Fut>(&mut self, offset: u64, send: F)
    where
        F: Fn(TerminalOutput) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = bool> + Send,
    {
        use base64::Engine as _;
        let (term, reader) = (self.term.clone(), self.reader);
        let mut rx = term.watch();
        self.task = Some(tokio::spawn(async move {
            let mut pos = offset;
            loop {
                // Seen before looking, so a wake during the look isn't lost.
                rx.borrow_and_update();
                match term.pull(reader, &mut pos) {
                    Pull::Data(offset, data, replay) => {
                        let out = TerminalOutput { terminal_id: term.id.clone(), offset, data: base64::engine::general_purpose::STANDARD.encode(&data), replay };
                        if !send(out).await {
                            break;
                        }
                    }
                    Pull::Wait => {
                        if rx.changed().await.is_err() {
                            break;
                        }
                    }
                    Pull::Gone => break,
                }
            }
        }));
    }
}

impl Drop for Attachment {
    fn drop(&mut self) {
        if let Some(t) = self.task.take() {
            t.abort();
        }
        self.term.detach(self.reader);
    }
}
