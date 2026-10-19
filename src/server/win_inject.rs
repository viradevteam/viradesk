//! Forwards mouse and keyboard injection into a process that can call SendInput.
//!
//! The installed server is LocalSystem in the interactive session. On this host
//! that process's `SendInput` returns ERROR_ACCESS_DENIED even after mouse and
//! keyboard permission are granted. A process launched from the same winlogon
//! token, on the current input desktop, can move the cursor. When a self-check
//! on the server thread cannot, events are sent to that helper.

use std::{
    ffi::{c_void, OsStr},
    fs::File,
    io::{Read, Write},
    os::windows::{ffi::OsStrExt, io::FromRawHandle, raw::HANDLE as RawHandle},
    ptr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::Duration,
};

use base::message_proto::{KeyEvent, MouseEvent};
use hbb_common::{
    anyhow::{anyhow, Result},
    log,
    protobuf::Message,
};
use windows::{
    core::{PCWSTR, PWSTR},
    Win32::{
        Foundation::{LocalFree, ERROR_PIPE_CONNECTED, HANDLE, HLOCAL, INVALID_HANDLE_VALUE},
        Security::{
            Authorization::{
                SetEntriesInAclW, EXPLICIT_ACCESS_W, SET_ACCESS, TRUSTEE_IS_SID, TRUSTEE_IS_USER,
                TRUSTEE_W,
            },
            CreateWellKnownSid, InitializeSecurityDescriptor, SetSecurityDescriptorDacl,
            WinLocalSystemSid, ACE_FLAGS, ACL, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_ALL_ACCESS, FILE_FLAGS_AND_ATTRIBUTES, FILE_GENERIC_READ,
            FILE_GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        },
        System::Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
        },
    },
};

const PIPE_NAME: &str = r"\\.\pipe\ViraDesk\inject";
const PIPE_ACCESS_DUPLEX: u32 = 0x00000003;
const KIND_PING: u8 = 0;
const KIND_MOUSE: u8 = 1;
const KIND_KEY: u8 = 2;

static HELPER_PROCESS: AtomicBool = AtomicBool::new(false);
static FORWARD: AtomicBool = AtomicBool::new(false);
static PIPE: Mutex<Option<File>> = Mutex::new(None);

pub fn run_helper() {
    HELPER_PROCESS.store(true, Ordering::SeqCst);
    if !crate::platform::windows::attach_input_desktop() {
        log::error!(
            "input helper failed to attach the input desktop: {}",
            std::io::Error::last_os_error()
        );
    }
    let moved = crate::platform::windows::prove_mouse_injection();
    log::info!("input helper self-check moved={}", moved);
    if let Err(err) = serve(moved) {
        log::error!("input helper stopped: {}", err);
    }
}

pub fn prepare_injection() {
    if HELPER_PROCESS.load(Ordering::SeqCst) {
        return;
    }
    if !crate::platform::windows::attach_input_desktop() {
        log::warn!(
            "server input thread failed to attach the input desktop: {}",
            std::io::Error::last_os_error()
        );
    }
    if crate::platform::windows::prove_mouse_injection() {
        log::info!("input injection stays in the server process; self-check moved the cursor");
        return;
    }
    log::warn!("input injection self-check did not move the cursor; starting session helper");
    match start_helper() {
        Ok(moved) => {
            FORWARD.store(true, Ordering::SeqCst);
            log::info!(
                "input injection helper is connected; helper self-check moved={}",
                moved
            );
        }
        Err(err) => log::error!("input injection helper failed: {}", err),
    }
}

pub fn forward_mouse(evt: &MouseEvent, conn: i32) -> bool {
    if HELPER_PROCESS.load(Ordering::SeqCst) || !FORWARD.load(Ordering::SeqCst) {
        return false;
    }
    let bytes = match evt.write_to_bytes() {
        Ok(bytes) => bytes,
        Err(err) => {
            log::error!("failed to encode mouse event for helper: {}", err);
            return true;
        }
    };
    if let Err(err) = write_frame(KIND_MOUSE, conn, &bytes) {
        log::error!("failed to forward mouse event: {}", err);
    }
    true
}

pub fn forward_key(evt: &KeyEvent) -> bool {
    if HELPER_PROCESS.load(Ordering::SeqCst) || !FORWARD.load(Ordering::SeqCst) {
        return false;
    }
    let bytes = match evt.write_to_bytes() {
        Ok(bytes) => bytes,
        Err(err) => {
            log::error!("failed to encode key event for helper: {}", err);
            return true;
        }
    };
    if let Err(err) = write_frame(KIND_KEY, 0, &bytes) {
        log::error!("failed to forward key event: {}", err);
    }
    true
}

fn start_helper() -> Result<bool> {
    let exe = std::env::current_exe()?;
    let cmd = format!("\"{}\" --win-input-inject", exe.display());
    let session = crate::platform::windows::get_current_process_session_id()
        .ok_or_else(|| anyhow!("no session id for input helper"))?;
    let handle = crate::platform::windows::launch_privileged_process(session, &cmd)?;
    if !handle.is_null() {
        unsafe { winapi::um::handleapi::CloseHandle(handle) };
    }
    let mut last_err = anyhow!("input helper pipe was not created");
    for _ in 0..50 {
        match open_client() {
            Ok(mut file) => {
                write_frame_to(&mut file, KIND_PING, 0, &[])?;
                let mut ack = [0u8; 1];
                file.read_exact(&mut ack)?;
                *PIPE.lock().unwrap() = Some(file);
                return Ok(ack[0] == 1);
            }
            Err(err) => {
                last_err = err;
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    Err(last_err)
}

fn serve(moved: bool) -> Result<()> {
    let handle = create_system_pipe()?;
    let connected = unsafe { ConnectNamedPipe(handle, None) };
    if connected.is_err() {
        let err = std::io::Error::last_os_error();
        let code = err.raw_os_error().unwrap_or(0);
        if code != ERROR_PIPE_CONNECTED.0 as i32 {
            return Err(anyhow!("ConnectNamedPipe failed: {}", err));
        }
    }
    let mut file = unsafe { File::from_raw_handle(handle.0 as RawHandle) };
    let mut header = [0u8; 9];
    loop {
        if !read_exact(&mut file, &mut header)? {
            log::info!("input helper pipe closed");
            return Ok(());
        }
        let kind = header[0];
        let conn = i32::from_le_bytes(header[1..5].try_into().unwrap());
        let len = u32::from_le_bytes(header[5..9].try_into().unwrap()) as usize;
        if len > 1024 * 1024 {
            return Err(anyhow!("input helper frame is too large: {}", len));
        }
        let mut body = vec![0u8; len];
        if len > 0 && !read_exact(&mut file, &mut body)? {
            return Err(anyhow!("input helper frame truncated"));
        }
        match kind {
            KIND_PING => file.write_all(&[if moved { 1 } else { 0 }])?,
            KIND_MOUSE => {
                if let Ok(evt) = MouseEvent::parse_from_bytes(&body) {
                    crate::server::input_service::handle_mouse_simulation_(&evt, conn);
                }
            }
            KIND_KEY => {
                if let Ok(evt) = KeyEvent::parse_from_bytes(&body) {
                    crate::server::input_service::handle_key_(&evt);
                }
            }
            _ => log::warn!("input helper ignored frame kind {}", kind),
        }
    }
}

fn write_frame(kind: u8, conn: i32, body: &[u8]) -> Result<()> {
    let mut guard = PIPE.lock().unwrap();
    let file = guard.as_mut().ok_or_else(|| anyhow!("input helper pipe is closed"))?;
    write_frame_to(file, kind, conn, body)
}

fn write_frame_to(file: &mut File, kind: u8, conn: i32, body: &[u8]) -> Result<()> {
    let mut header = [0u8; 9];
    header[0] = kind;
    header[1..5].copy_from_slice(&conn.to_le_bytes());
    header[5..9].copy_from_slice(&(body.len() as u32).to_le_bytes());
    file.write_all(&header)?;
    if !body.is_empty() {
        file.write_all(body)?;
    }
    file.flush()?;
    Ok(())
}

fn read_exact(file: &mut File, buf: &mut [u8]) -> Result<bool> {
    let mut pos = 0;
    while pos < buf.len() {
        match file.read(&mut buf[pos..]) {
            Ok(0) => return Ok(false),
            Ok(n) => pos += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err.into()),
        }
    }
    Ok(true)
}

fn open_client() -> Result<File> {
    let wide: Vec<u16> = OsStr::new(PIPE_NAME)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let handle = unsafe {
        CreateFileW(
            PCWSTR::from_raw(wide.as_ptr()),
            FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    };
    match handle {
        Ok(handle) => Ok(unsafe { File::from_raw_handle(handle.0 as RawHandle) }),
        Err(err) => Err(anyhow!("open input helper pipe: {}", err)),
    }
}

fn create_system_pipe() -> Result<HANDLE> {
    let mut system_sid = vec![0u8; 64];
    let mut system_sid_size = system_sid.len() as u32;
    unsafe {
        CreateWellKnownSid(
            WinLocalSystemSid,
            None,
            Some(PSID(system_sid.as_mut_ptr() as *mut c_void)),
            &mut system_sid_size,
        )
        .map_err(|err| anyhow!("CreateWellKnownSid: {}", err))?;
    }
    let mut explicit_access: [EXPLICIT_ACCESS_W; 1] = unsafe { std::mem::zeroed() };
    explicit_access[0].grfAccessPermissions = FILE_ALL_ACCESS.0;
    explicit_access[0].grfAccessMode = SET_ACCESS;
    explicit_access[0].grfInheritance = ACE_FLAGS(0);
    explicit_access[0].Trustee = TRUSTEE_W {
        pMultipleTrustee: ptr::null_mut(),
        MultipleTrusteeOperation: Default::default(),
        TrusteeForm: TRUSTEE_IS_SID,
        TrusteeType: TRUSTEE_IS_USER,
        ptstrName: PWSTR::from_raw(system_sid.as_ptr() as *mut u16),
    };
    let mut new_acl: *mut ACL = ptr::null_mut();
    let acl_result = unsafe { SetEntriesInAclW(Some(&explicit_access), None, &mut new_acl) };
    if acl_result.0 != 0 || new_acl.is_null() {
        return Err(anyhow!("SetEntriesInAclW failed: {}", acl_result.0));
    }
    let mut sd_buffer = [0u8; 64];
    let sd_ptr = PSECURITY_DESCRIPTOR(sd_buffer.as_mut_ptr() as *mut c_void);
    unsafe {
        InitializeSecurityDescriptor(sd_ptr, 1)
            .map_err(|err| anyhow!("InitializeSecurityDescriptor: {}", err))?;
        SetSecurityDescriptorDacl(sd_ptr, true, Some(new_acl as *const _ as *const _), false)
            .map_err(|err| anyhow!("SetSecurityDescriptorDacl: {}", err))?;
    }
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd_buffer.as_mut_ptr() as *mut c_void,
        bInheritHandle: false.into(),
    };
    let wide: Vec<u16> = OsStr::new(PIPE_NAME)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR::from_raw(wide.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(PIPE_ACCESS_DUPLEX),
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            1,
            64 * 1024,
            64 * 1024,
            0,
            Some(&sa),
        )
    };
    unsafe {
        let _ = LocalFree(Some(HLOCAL(new_acl as *mut c_void)));
    }
    if handle == INVALID_HANDLE_VALUE {
        return Err(anyhow!(
            "CreateNamedPipeW failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(handle)
}
