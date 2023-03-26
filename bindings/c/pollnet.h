#include <stdint.h>
#include <stdbool.h>

typedef struct pollnet_ctx pollnet_ctx;
typedef uint64_t sockethandle_t;
typedef uint32_t socketstatus_t;

pollnet_ctx* pollnet_init();
pollnet_ctx* pollnet_get_or_init_static();
void pollnet_shutdown(pollnet_ctx* ctx);
sockethandle_t pollnet_open_tcp(pollnet_ctx* ctx, const char* addr);
sockethandle_t pollnet_listen_tcp(pollnet_ctx* ctx, const char* addr);
sockethandle_t pollnet_open_ws(pollnet_ctx* ctx, const char* url);
sockethandle_t pollnet_simple_http_get(pollnet_ctx* ctx, const char* url, bool ret_body_only);
sockethandle_t pollnet_simple_http_post(pollnet_ctx* ctx, const char* url, bool ret_body_only, const char* content_type, const char* data, uint32_t datasize);
void pollnet_close(pollnet_ctx* ctx, sockethandle_t handle);
void pollnet_close_all(pollnet_ctx* ctx);
void pollnet_send(pollnet_ctx* ctx, sockethandle_t handle, const char* msg);
void pollnet_send_binary(pollnet_ctx* ctx, sockethandle_t handle, const unsigned char* msg, uint32_t msgsize);
socketstatus_t pollnet_update(pollnet_ctx* ctx, sockethandle_t handle);
socketstatus_t pollnet_update_blocking(pollnet_ctx* ctx, sockethandle_t handle);
int32_t pollnet_get(pollnet_ctx* ctx, sockethandle_t handle, char* dest, uint32_t dest_size);
int32_t pollnet_get_error(pollnet_ctx* ctx, sockethandle_t handle, char* dest, uint32_t dest_size);
sockethandle_t pollnet_get_connected_client_handle(pollnet_ctx* ctx, sockethandle_t handle);
sockethandle_t pollnet_listen_ws(pollnet_ctx* ctx, const char* addr);
sockethandle_t pollnet_serve_static_http(pollnet_ctx* ctx, const char* addr, const char* serve_dir);
sockethandle_t pollnet_serve_http(pollnet_ctx* ctx, const char* addr);
void pollnet_add_virtual_file(pollnet_ctx* ctx, sockethandle_t handle, const char* filename, const char* filedata, uint32_t filesize);
void pollnet_remove_virtual_file(pollnet_ctx* ctx, sockethandle_t handle, const char* filename);
int32_t pollnet_get_nanoid(char* dest, uint32_t dest_size);
void pollnet_sleep_ms(uint32_t milliseconds);
