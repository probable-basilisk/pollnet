mod context;
use context::PollnetContext;
use context::SocketHandle;
use context::SocketStatus;
use slotmap::Key;

use log::{error, info, warn};
use std::ffi::CStr;
use std::os::raw::c_char;
use std::thread;
use std::time;

const VERSION_STR: &str = concat!(env!("CARGO_PKG_VERSION"), "\0");

fn c_str_to_string(s: *const c_char) -> String {
    unsafe { CStr::from_ptr(s).to_string_lossy().into_owned() }
}

fn c_data_to_vec(data: *const u8, datasize: u32) -> Vec<u8> {
    unsafe { std::slice::from_raw_parts(data, datasize as usize).to_vec() }
}

#[no_mangle]
pub extern "C" fn pollnet_version() -> *const c_char {
    VERSION_STR.as_ptr() as *const c_char
}

#[no_mangle]
pub extern "C" fn pollnet_init() -> *mut PollnetContext {
    Box::into_raw(Box::new(PollnetContext::new()))
}

#[no_mangle]
pub extern "C" fn pollnet_handle_is_valid(handle: u64) -> bool {
    let handle: SocketHandle = handle.into();
    handle.is_null()
}

#[no_mangle]
pub extern "C" fn pollnet_invalid_handle() -> u64 {
    SocketHandle::null().into()
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_shutdown(ctx: *mut PollnetContext) {
    info!("Requested ctx close!");
    let ctx = unsafe { &mut *ctx };
    ctx.shutdown();

    // take ownership and drop
    let b = unsafe { Box::from_raw(ctx) };
    drop(b);
    info!("Everything should be dead now!");
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_open_ws(ctx: *mut PollnetContext, url: *const c_char) -> u64 {
    let ctx = unsafe { &mut *ctx };
    let url = c_str_to_string(url);
    ctx.open_ws(url).into()
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_listen_ws(ctx: *mut PollnetContext, addr: *const c_char) -> u64 {
    let ctx = unsafe { &mut *ctx };
    let addr = c_str_to_string(addr);
    ctx.listen_ws(addr).into()
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_open_tcp(ctx: *mut PollnetContext, addr: *const c_char) -> u64 {
    let ctx = unsafe { &mut *ctx };
    let addr = c_str_to_string(addr);
    ctx.open_tcp(addr).into()
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_listen_tcp(ctx: *mut PollnetContext, addr: *const c_char) -> u64 {
    let ctx = unsafe { &mut *ctx };
    let addr = c_str_to_string(addr);
    ctx.listen_tcp(addr).into()
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_simple_http_get(
    ctx: *mut PollnetContext,
    url: *const c_char,
    headers: *const c_char,
    ret_body_only: bool,
) -> u64 {
    let ctx = unsafe { &mut *ctx };
    let url = c_str_to_string(url);
    let headers = c_str_to_string(headers);
    ctx.open_http_get_simple(url, headers, ret_body_only).into()
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_simple_http_post(
    ctx: *mut PollnetContext,
    url: *const c_char,
    headers: *const c_char,
    bodydata: *const u8,
    bodysize: u32,
    ret_body_only: bool,
) -> u64 {
    let ctx = unsafe { &mut *ctx };
    let url = c_str_to_string(url);
    let headers = c_str_to_string(headers);
    let body = c_data_to_vec(bodydata, bodysize);
    ctx.open_http_post_simple(url, headers, body, ret_body_only)
        .into()
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_serve_static_http(
    ctx: *mut PollnetContext,
    addr: *const c_char,
    serve_dir: *const c_char,
) -> u64 {
    let ctx = unsafe { &mut *ctx };
    let addr = c_str_to_string(addr);
    let serve_dir = c_str_to_string(serve_dir);
    ctx.serve_http(addr, Some(serve_dir)).into()
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_serve_http(ctx: *mut PollnetContext, addr: *const c_char) -> u64 {
    let ctx = unsafe { &mut *ctx };
    let addr = c_str_to_string(addr);
    ctx.serve_http(addr, None).into()
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_serve_dynamic_http(
    ctx: *mut PollnetContext,
    addr: *const c_char,
    keep_alive: bool
) -> u64 {
    let ctx = unsafe { &mut *ctx };
    let addr = c_str_to_string(addr);
    ctx.serve_http_dynamic(addr, keep_alive).into()
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_close(ctx: *mut PollnetContext, handle: u64) {
    let ctx = unsafe { &mut *ctx };
    ctx.close(handle.into())
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_close_all(ctx: *mut PollnetContext) {
    let ctx = unsafe { &mut *ctx };
    ctx.close_all()
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_status(ctx: *mut PollnetContext, handle: u64) -> u32 {
    let ctx = unsafe { &*ctx };
    if let Some(socket) = ctx.sockets.get(handle.into()) {
        socket.status
    } else {
        SocketStatus::InvalidHandle
    }
    .into()
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_send(ctx: *mut PollnetContext, handle: u64, msg: *const c_char) {
    let ctx = unsafe { &mut *ctx };
    let msg = c_str_to_string(msg);
    ctx.send(handle.into(), msg)
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_send_binary(
    ctx: *mut PollnetContext,
    handle: u64,
    msg: *const u8,
    msgsize: u32,
) {
    let ctx = unsafe { &mut *ctx };
    let msg = c_data_to_vec(msg, msgsize);
    ctx.send_binary(handle.into(), msg)
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_add_virtual_file(
    ctx: *mut PollnetContext,
    handle: u64,
    filename: *const c_char,
    filedata: *const u8,
    datasize: u32,
) {
    let ctx = unsafe { &mut *ctx };
    let filename = c_str_to_string(filename);
    let filedata = c_data_to_vec(filedata, datasize);
    ctx.add_virtual_file(handle.into(), filename, filedata)
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_remove_virtual_file(
    ctx: *mut PollnetContext,
    handle: u64,
    filename: *const c_char,
) {
    let ctx = unsafe { &mut *ctx };
    let filename = c_str_to_string(filename);
    ctx.remove_virtual_file(handle.into(), filename)
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_update(ctx: *mut PollnetContext, handle: u64) -> u32 {
    let ctx = unsafe { &mut *ctx };
    ctx.update(handle.into(), false).into()
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_update_blocking(ctx: *mut PollnetContext, handle: u64) -> u32 {
    let ctx = unsafe { &mut *ctx };
    ctx.update(handle.into(), true).into()
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_get_data_size(ctx: *mut PollnetContext, handle: u64) -> u32 {
    let ctx = unsafe { &mut *ctx };
    let socket = match ctx.sockets.get(handle.into()) {
        Some(socket) => socket,
        None => return 0,
    };
    let msgsize = match &socket.data {
        Some(msg) => msg.len(),
        None => 0,
    };
    if msgsize > u32::MAX as usize {
        error!("Message size exceeds u32 max: {:}", msgsize);
        return 0;
    };
    msgsize as u32
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_get_data(
    ctx: *mut PollnetContext,
    handle: u64,
    dest: *mut u8,
    dest_size: u32,
) -> u32 {
    let ctx = unsafe { &mut *ctx };
    let socket = match ctx.sockets.get(handle.into()) {
        Some(socket) => socket,
        None => return 0,
    };

    let msgsize = match &socket.data {
        Some(msg) => msg.len(),
        None => 0,
    };

    if msgsize > u32::MAX as usize {
        error!("Message size exceeds u32 max: {:}", msgsize);
        return 0;
    }

    if msgsize > dest_size as usize {
        return msgsize as u32;
    }

    match &socket.data {
        Some(msg) => {
            let ncopy = msg.len().min(dest_size as usize);
            unsafe {
                std::ptr::copy_nonoverlapping(msg.as_ptr(), dest, ncopy);
            }
            ncopy as u32
        }
        None => 0,
    }
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_unsafe_get_data_ptr(
    ctx: *mut PollnetContext,
    handle: u64,
) -> *const u8 {
    let ctx = unsafe { &mut *ctx };
    let socket = match ctx.sockets.get_mut(handle.into()) {
        Some(socket) => socket,
        None => return std::ptr::null(),
    };
    match &socket.data {
        Some(msg) => msg.as_ptr(),
        None => std::ptr::null(),
    }
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_clear_data(ctx: *mut PollnetContext, handle: u64) {
    let ctx = unsafe { &mut *ctx };
    if let Some(socket) = ctx.sockets.get_mut(handle.into()) {
        socket.data.take();
    }
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_get_connected_client_handle(
    ctx: *mut PollnetContext,
    handle: u64,
) -> u64 {
    let ctx = unsafe { &mut *ctx };
    match ctx.sockets.get_mut(handle.into()) {
        Some(socket) => socket.last_client_handle,
        None => SocketHandle::null(),
    }
    .into()
}

static mut HACKSTATICCONTEXT: *mut PollnetContext = 0 as *mut PollnetContext;

/// # Safety
///
/// This function is a last resort.
/// Please actually keep track of a ctx* if at all possible!
#[no_mangle]
pub unsafe extern "C" fn pollnet_get_or_init_static() -> *mut PollnetContext {
    if HACKSTATICCONTEXT.is_null() {
        warn!("INITIALIZING HACK STATIC CONTEXT");
        HACKSTATICCONTEXT = Box::into_raw(Box::new(PollnetContext::new()))
    }
    HACKSTATICCONTEXT
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_get_nanoid(dest: *mut u8, dest_size: u32) -> u32 {
    let id = nanoid::nanoid!();
    let ncopy = id.len().min(dest_size as usize);
    unsafe {
        std::ptr::copy_nonoverlapping(id.as_ptr(), dest, ncopy);
    }
    ncopy as u32
}

/// # Safety
///
/// ctx must be valid
#[no_mangle]
pub unsafe extern "C" fn pollnet_sleep_ms(milliseconds: u32) {
    let dur = time::Duration::from_millis(milliseconds.into());
    thread::sleep(dur);
}
