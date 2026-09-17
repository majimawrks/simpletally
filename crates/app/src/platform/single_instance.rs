//! Single-instance election + activation (PHASE0 gate item 3).
//!
//! Election is by **mutex ownership** (not ERROR_ALREADY_EXISTS, which is wrong — a
//! mutex outlives its owner while any handle is open, and a dead owner leaves an
//! abandoned-but-present mutex). Activation is a tiny message over a named pipe: a
//! second launch connects, writes 2 bytes, and exits without ever opening the DB; the
//! primary's pipe-server thread forwards it as `UserEvent::Activate`.
//!
//! Identity is derived from the executable's directory (the DB lives beside the exe),
//! so "one instance per install" — a second copy in another folder is independent.
//! The mutex is `Global\` so one install is one instance across sessions; the pipe
//! namespace is already machine-global.
//!
//! Spike scope: default pipe security (single-user machine) and a blocking server
//! thread that dies with the process. PHASE0 notes the hardening left for later:
//! a per-user DACL and overlapped, cancellable I/O.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};
use std::thread;
use std::time::Duration;

use winit::event_loop::EventLoopProxy;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, OPEN_EXISTING,
    PIPE_ACCESS_INBOUND,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_MESSAGE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE,
};
use windows_sys::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};

use super::event::{ActivationTarget, UserEvent};

const PROTOCOL_VERSION: u8 = 1;

pub enum Election {
    /// We are the only/primary instance.
    Primary(Primary),
    /// Another instance is already running; we've signalled it and should exit.
    Secondary,
}

/// Held by the primary for the process lifetime: owns the mutex and knows the pipe
/// name so it can start the activation server once the event loop exists.
pub struct Primary {
    mutex: HANDLE,
    pipe_name: Vec<u16>,
}

impl Primary {
    /// Start the pipe server that forwards second-instance activations to the loop.
    pub fn serve(&self, proxy: EventLoopProxy<UserEvent>) {
        let pipe_name = self.pipe_name.clone();
        thread::Builder::new()
            .name("single-instance-pipe".into())
            .spawn(move || pipe_server_loop(&pipe_name, proxy))
            .expect("spawn pipe server thread");
    }
}

impl Drop for Primary {
    fn drop(&mut self) {
        if !self.mutex.is_null() {
            // SAFETY: mutex handle we own; released on process teardown.
            unsafe { CloseHandle(self.mutex) };
        }
    }
}

/// Elect this process as primary or secondary. A secondary signals the primary with
/// `activation` before returning.
pub fn elect(activation: ActivationTarget) -> Election {
    let id = instance_id();
    let mutex_name = wide(&format!("Global\\SimpleTally.{id}"));
    let pipe_name = wide(&format!("\\\\.\\pipe\\SimpleTally.{id}"));

    // SAFETY: FFI; names are valid null-terminated UTF-16.
    let mutex = unsafe { CreateMutexW(null(), 0, mutex_name.as_ptr()) };
    if mutex.is_null() {
        // Couldn't create the mutex — fail open: run as primary without the guarantee.
        eprintln!("single-instance: CreateMutexW failed; continuing without single-instance");
        return Election::Primary(Primary {
            mutex: null_mut(),
            pipe_name,
        });
    }

    // Election by ownership: acquired (or abandoned by a dead owner) => primary;
    // timed out => someone else owns it => secondary; anything else (WAIT_FAILED) is
    // indeterminate, so fail open as primary rather than silently not launching.
    let wait = unsafe { WaitForSingleObject(mutex, 0) };
    if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
        Election::Primary(Primary { mutex, pipe_name })
    } else if wait == WAIT_TIMEOUT {
        signal_primary(&pipe_name, activation);
        unsafe { CloseHandle(mutex) };
        Election::Secondary
    } else {
        let err = unsafe { GetLastError() };
        eprintln!("single-instance: WaitForSingleObject -> 0x{wait:x} (err {err}); running as primary");
        Election::Primary(Primary { mutex, pipe_name })
    }
}

/// Secondary path: connect to the primary's pipe and send the 2-byte activation.
fn signal_primary(pipe_name: &[u16], activation: ActivationTarget) {
    let target = match activation {
        ActivationTarget::Main => 0u8,
        ActivationTarget::QuickAdd => 1u8,
    };
    let msg = [PROTOCOL_VERSION, target];

    // The primary may be mid-startup; retry briefly.
    for _ in 0..10 {
        // SAFETY: FFI; pipe_name is null-terminated.
        let h = unsafe {
            CreateFileW(
                pipe_name.as_ptr(),
                GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                0,
                null_mut(),
            )
        };
        if h != INVALID_HANDLE_VALUE && !h.is_null() {
            let mut written = 0u32;
            let ok = unsafe { WriteFile(h, msg.as_ptr(), msg.len() as u32, &mut written, null_mut()) };
            unsafe { CloseHandle(h) };
            if ok != 0 && written as usize == msg.len() {
                return;
            }
            // Write failed or was short — fall through and retry.
        }
        thread::sleep(Duration::from_millis(150));
    }
    eprintln!("single-instance: another instance is running but did not respond");
}

/// Primary path: accept connections and forward activations. Runs until the process
/// exits (the blocking accept is fine to abandon on teardown for the spike).
fn pipe_server_loop(pipe_name: &[u16], proxy: EventLoopProxy<UserEvent>) {
    // SAFETY: FFI; one reusable message-mode instance, local clients only.
    let pipe = unsafe {
        CreateNamedPipeW(
            pipe_name.as_ptr(),
            PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_REJECT_REMOTE_CLIENTS,
            1,  // a single instance is enough
            16, // out buffer
            16, // in buffer
            0,  // default timeout
            null(),
        )
    };
    if pipe == INVALID_HANDLE_VALUE {
        eprintln!("single-instance: CreateNamedPipeW failed (endpoint in use?); activation disabled");
        return;
    }

    loop {
        // SAFETY: FFI on our own pipe handle.
        let connected = unsafe { ConnectNamedPipe(pipe, null_mut()) };
        let err = unsafe { GetLastError() };
        // ERROR_PIPE_CONNECTED: client connected before we called Connect.
        // ERROR_NO_DATA: client already wrote and closed — its bytes may still be
        // buffered, so we still attempt the read rather than discard the request.
        let ok = connected != 0 || err == ERROR_PIPE_CONNECTED || err == ERROR_NO_DATA;
        if ok {
            let mut buf = [0u8; 2];
            let mut read = 0u32;
            let r = unsafe { ReadFile(pipe, buf.as_mut_ptr(), buf.len() as u32, &mut read, null_mut()) };
            if r != 0 && read >= 2 && buf[0] == PROTOCOL_VERSION {
                let target = if buf[1] == 1 {
                    ActivationTarget::QuickAdd
                } else {
                    ActivationTarget::Main
                };
                let _ = proxy.send_event(UserEvent::Activate(target));
            }
        }
        unsafe { DisconnectNamedPipe(pipe) };
    }
}

/// Stable per-install id: a hash of the executable's (canonical, lowercased) directory.
fn instance_id() -> String {
    use std::hash::{Hash, Hasher};
    let dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .map(|d| std::fs::canonicalize(&d).unwrap_or(d))
        .map(|d| d.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    dir.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}
