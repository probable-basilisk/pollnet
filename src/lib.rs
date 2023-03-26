mod context;
use context::PollnetContext;
use context::SocketHandle;
use context::SocketStatus;
use slotmap::Key;

use log::{info, warn};
use std::ffi::CStr;
use std::os::raw::c_char;
use std::thread;
use std::time;

fn c_str_to_string(s: *const c_char) -> String {
    unsafe { CStr::from_ptr(s).to_string_lossy().into_owned() }
}

fn c_data_to_vec(data: *const u8, datasize: u32) -> Vec<u8> {
    unsafe { std::slice::from_raw_parts(data, datasize as usize).to_vec() }
}

#[no_mangle]
pub extern "C" fn pollnet_init() -> *mut PollnetContext {
    Box::into_raw(Box::new(PollnetContext::new()))
}

#[no_mangle]
pub extern "C" fn pollnet_shutdown(ctx: *mut PollnetContext) {
    info!("Requested ctx close!");
    let ctx = unsafe { &mut *ctx };
    ctx.shutdown();

    // take ownership and drop
    let b = unsafe { Box::from_raw(ctx) };
    drop(b);
    info!("Everything should be dead now!");
}

#[no_mangle]
pub extern "C" fn pollnet_open_ws(ctx: *mut PollnetContext, url: *const c_char) -> u64 {
    let ctx = unsafe { &mut *ctx };
    let url = c_str_to_string(url);
    ctx.open_ws(url).into()
}

#[no_mangle]
pub extern "C" fn pollnet_listen_ws(ctx: *mut PollnetContext, addr: *const c_char) -> u64 {
    let ctx = unsafe { &mut *ctx };
    let addr = c_str_to_string(addr);
    ctx.listen_ws(addr).into()
}

#[no_mangle]
pub extern "C" fn pollnet_open_tcp(ctx: *mut PollnetContext, addr: *const c_char) -> u64 {
    let ctx = unsafe { &mut *ctx };
    let addr = c_str_to_string(addr);
    ctx.open_tcp(addr).into()
}

#[no_mangle]
pub extern "C" fn pollnet_listen_tcp(ctx: *mut PollnetContext, addr: *const c_char) -> u64 {
    let ctx = unsafe { &mut *ctx };
    let addr = c_str_to_string(addr);
    ctx.listen_tcp(addr).into()
}

#[no_mangle]
pub extern "C" fn pollnet_simple_http_get(
    ctx: *mut PollnetContext,
    addr: *const c_char,
    body_only: bool,
) -> u64 {
    let ctx = unsafe { &mut *ctx };
    let addr = c_str_to_string(addr);
    ctx.open_http_get_simple(addr, body_only).into()
}

#[no_mangle]
pub extern "C" fn pollnet_simple_http_post(
    ctx: *mut PollnetContext,
    addr: *const c_char,
    body_only: bool,
    content_type: *const c_char,
    bodydata: *const u8,
    bodysize: u32,
) -> u64 {
    let ctx = unsafe { &mut *ctx };
    let addr = c_str_to_string(addr);
    let content_type = c_str_to_string(content_type);
    let body = c_data_to_vec(bodydata, bodysize);
    ctx.open_http_post_simple(addr, body_only, content_type, body)
        .into()
}

#[no_mangle]
pub extern "C" fn pollnet_serve_static_http(
    ctx: *mut PollnetContext,
    addr: *const c_char,
    serve_dir: *const c_char,
) -> u64 {
    let ctx = unsafe { &mut *ctx };
    let addr = c_str_to_string(addr);
    let serve_dir = c_str_to_string(serve_dir);
    ctx.serve_http(addr, Some(serve_dir)).into()
}

#[no_mangle]
pub extern "C" fn pollnet_serve_http(ctx: *mut PollnetContext, addr: *const c_char) -> u64 {
    let ctx = unsafe { &mut *ctx };
    let addr = c_str_to_string(addr);
    ctx.serve_http(addr, None).into()
}

#[no_mangle]
pub extern "C" fn pollnet_close(ctx: *mut PollnetContext, handle: u64) {
    let ctx = unsafe { &mut *ctx };
    ctx.close(handle.into())
}

#[no_mangle]
pub extern "C" fn pollnet_close_all(ctx: *mut PollnetContext) {
    let ctx = unsafe { &mut *ctx };
    ctx.close_all()
}

#[no_mangle]
pub extern "C" fn pollnet_status(ctx: *mut PollnetContext, handle: u64) -> u32 {
    let ctx = unsafe { &*ctx };
    if let Some(socket) = ctx.sockets.get(handle.into()) {
        socket.status
    } else {
        SocketStatus::INVALIDHANDLE
    }
    .into()
}

#[no_mangle]
pub extern "C" fn pollnet_send(ctx: *mut PollnetContext, handle: u64, msg: *const c_char) {
    let ctx = unsafe { &mut *ctx };
    let msg = c_str_to_string(msg);
    ctx.send(handle.into(), msg)
}

#[no_mangle]
pub extern "C" fn pollnet_send_binary(
    ctx: *mut PollnetContext,
    handle: u64,
    msg: *const u8,
    msgsize: u32,
) {
    let ctx = unsafe { &mut *ctx };
    let msg = c_data_to_vec(msg, msgsize);
    ctx.send_binary(handle.into(), msg)
}

#[no_mangle]
pub extern "C" fn pollnet_add_virtual_file(
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

#[no_mangle]
pub extern "C" fn pollnet_remove_virtual_file(
    ctx: *mut PollnetContext,
    handle: u64,
    filename: *const c_char,
) {
    let ctx = unsafe { &mut *ctx };
    let filename = c_str_to_string(filename);
    ctx.remove_virtual_file(handle.into(), filename)
}

#[no_mangle]
pub extern "C" fn pollnet_update(ctx: *mut PollnetContext, handle: u64) -> u32 {
    let ctx = unsafe { &mut *ctx };
    ctx.update(handle.into(), false).into()
}

#[no_mangle]
pub extern "C" fn pollnet_update_blocking(ctx: *mut PollnetContext, handle: u64) -> u32 {
    let ctx = unsafe { &mut *ctx };
    ctx.update(handle.into(), true).into()
}

#[no_mangle]
pub extern "C" fn pollnet_get(
    ctx: *mut PollnetContext,
    handle: u64,
    dest: *mut u8,
    dest_size: u32,
) -> i32 {
    let ctx = unsafe { &mut *ctx };
    let socket = match ctx.sockets.get_mut(handle.into()) {
        Some(socket) => socket,
        None => return -1,
    };

    match socket.message.take() {
        Some(msg) => {
            let ncopy = msg.len();
            if ncopy < (dest_size as usize) {
                unsafe {
                    std::ptr::copy_nonoverlapping(msg.as_ptr(), dest, ncopy);
                }
                ncopy as i32
            } else {
                0
            }
        }
        None => 0,
    }
}

#[no_mangle]
pub extern "C" fn pollnet_get_connected_client_handle(
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

#[no_mangle]
pub extern "C" fn pollnet_get_error(
    ctx: *mut PollnetContext,
    handle: u64,
    dest: *mut u8,
    dest_size: u32,
) -> i32 {
    let ctx = unsafe { &mut *ctx };
    let socket = match ctx.sockets.get_mut(handle.into()) {
        Some(socket) => socket,
        None => return -1,
    };

    match socket.error.take() {
        Some(msg) => {
            let ncopy = msg.len();
            if ncopy < (dest_size as usize) {
                unsafe {
                    std::ptr::copy_nonoverlapping(msg.as_ptr(), dest, ncopy);
                }
                ncopy as i32
            } else {
                0
            }
        }
        None => 0,
    }
}

static mut HACKSTATICCONTEXT: *mut PollnetContext = 0 as *mut PollnetContext;

#[no_mangle]
pub unsafe extern "C" fn pollnet_get_or_init_static() -> *mut PollnetContext {
    if HACKSTATICCONTEXT.is_null() {
        warn!("INITIALIZING HACK STATIC CONTEXT");
        HACKSTATICCONTEXT = Box::into_raw(Box::new(PollnetContext::new()))
    }
    HACKSTATICCONTEXT
}

#[no_mangle]
pub extern "C" fn pollnet_get_nanoid(dest: *mut u8, dest_size: u32) -> i32 {
    let id = nanoid::nanoid!();
    if id.len() < (dest_size as usize) {
        unsafe {
            std::ptr::copy_nonoverlapping(id.as_ptr(), dest, id.len());
        }
        id.len() as i32
    } else {
        0
    }
}

#[no_mangle]
pub extern "C" fn pollnet_sleep_ms(milliseconds: u32) {
    let dur = time::Duration::from_millis(milliseconds.into());
    thread::sleep(dur);
}
