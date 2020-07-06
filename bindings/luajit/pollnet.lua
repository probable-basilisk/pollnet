--[[ 
pollnet bindings for luajit

example usage to read twitch chat:
local pollnet = require("pollnet")
local async = require("async") -- assuming you have some kind of async
async.run(function()
  local url = "wss://irc-ws.chat.twitch.tv:443"
  local sock = pollnet.open_ws(url)
  sock:send("PASS doesntmatter")
  -- special nick for anon read-only access on twitch
  local anon_user_name = "justinfan" .. math.random(1, 100000)
  local target_channel = "your_channel_name_here"
  sock:send("NICK " .. anon_user_name)
  sock:send("JOIN #" .. target_channel)
  
  while sock:poll() do
    local msg = sock:last_message()
    if msg then
      if msg == "PING :tmi.twitch.tv" then
        sock:send("PONG :tmi.twitch.tv")
      end
      print(msg) 
    end
    async.await_frames(1)
  end
  print("Socket closed: ", sock:last_message())
end)
]]

local ffi = require("ffi")
ffi.cdef[[
struct pnctx* pollnet_init();
void pollnet_shutdown(struct pnctx* ctx);
unsigned int pollnet_open_ws(struct pnctx* ctx, const char* url);
void pollnet_close(struct pnctx* ctx, unsigned int handle);
void pollnet_send(struct pnctx* ctx, unsigned int handle, const char* msg);
unsigned int pollnet_update(struct pnctx* ctx, unsigned int handle);
int pollnet_get(struct pnctx* ctx, unsigned int handle, char* dest, unsigned int dest_size);
int pollnet_get_error(struct pnctx* ctx, unsigned int handle, char* dest, unsigned int dest_size);
]]

local POLLNET_RESULT_CODES = {
  [0] = "invalid_handle",
  [1] = "closed",
  [2] = "opening",
  [3] = "nodata",
  [4] = "hasdata",
  [5] = "error",
}

local pollnet = ffi.load("pollnet")
local _ctx = nil

local function init_ctx()
  if _ctx then return end
  _ctx = pollnet.pollnet_init()
  assert(_ctx ~= nil)
end

local function shutdown_ctx()
  if not _ctx then return end
  pollnet.pollnet_shutdown(_ctx)
  _ctx = nil
end

local socket_mt = {}
function socket_mt:_open(scratch_size, opener, ...)
  init_ctx()
  if self._socket then self:close() end
  if not scratch_size then scratch_size = 64000 end
  self._socket = opener(_ctx, ...) --pollnet.pollnet_open(url)
  self._scratch = ffi.new("int8_t[?]", scratch_size)
  self._scratch_size = scratch_size
  self._status = "unpolled"
end

function socket_mt:open_ws(url, scratch_size)
  self:_open(scratch_size, pollnet.pollnet_open_ws, url)
end

function socket_mt:poll()
  if not self._socket then 
    self._status = "invalid"
    return false, "invalid"
  end
  local res = POLLNET_RESULT_CODES[pollnet.pollnet_update(_ctx, self._socket)] or "error"
  self._status = res
  self._last_message = nil
  if res == "hasdata" then
    self._status = "open"
    local msg_size = pollnet.pollnet_get(_ctx, self._socket, self._scratch, self._scratch_size)
    if msg_size > 0 then
      self._last_message = ffi.string(self._scratch, msg_size)
    end
    return true, self._last_message
  elseif res == "nodata" then
    self._status = "open"
    return true
  elseif res == "opening" then
    self._status = "opening"
    return true
  elseif res == "error" then
    self._last_message = self:error_msg()
    self._socket = nil -- hmm
    return false, self._last_message
  elseif res == "closed" then
    return false, "closed"
  end
end

function socket_mt:last_message()
  return self._last_message
end
function socket_mt:status()
  return self._status
end
function socket_mt:send(msg)
  assert(self._socket)
  pollnet.pollnet_send(_ctx, self._socket, msg)
end
function socket_mt:close()
  assert(self._socket)
  pollnet.pollnet_close(_ctx, self._socket)
  self._socket = nil
end
function socket_mt:error_msg()
  if not self._socket then return "No socket!" end
  local msg_size = pollnet.pollnet_get_error(_ctx, self._socket, self._scratch, self._scratch_size)
  if msg_size > 0 then
    local smsg = ffi.string(self._scratch, msg_size)
    return smsg
  else
    return nil
  end
end

local function open_ws(url, scratch_size)
  local socket = setmetatable({}, {__index = socket_mt})
  socket:open_ws(url, scratch_size)
  return socket
end

return {init = init_ctx, shutdown = shutdown_ctx, open_ws = open_ws, pollnet = pollnet}