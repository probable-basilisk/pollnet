# pollnet
Compact polling-based C-API networking library designed to be easily used from
within game-embedded LuaJIT environments (e.g., mods)

# Features
* Websocket client and server (both ws:// and wss:// for clients)
* TCP client and server
* bare-bones HTTP client: simple GET/POST
* bare-bones HTTP server: serve static files from disk or from memory

# Usage (luajit bindings)
```Lua
-- pollnet.lua FFI loads pollnet.dll
local pollnet = require("pollnet") 

local sock = pollnet.open_ws("wss://irc-ws.chat.twitch.tv:443")
-- special nick for anon read-only access on twitch
sock:send("PASS doesntmatter")
sock:send("NICK justinfan" .. math.random(1, 100000))
sock:send("JOIN #some_twitch_channel")

-- assuming that somehow you can run a callback on each frame,
-- or on some reasonable timer
each_game_tick(function()
  if not sock then return end
  local happy, msg = sock:poll()
  if not happy then
    sock:close() -- good form to avoid keeping sockets open
    sock = nil
    return
  end
  if msg then
    if msg == "PING :tmi.twitch.tv" then
      sock:send("PONG :tmi.twitch.tv")
    end
    print("CHAT: " .. msg) 
  end
end)

-- HTTP requests work roughly the same way, except you
-- only get three messages back: status code, headers, and body
local req_sock = pollnet.http_get("https://www.example.com")
local part_order = {"STATUS CODE:", "HEADERS:", "BODY:"}
local parts = {}
each_game_tick(function()
  if not req_sock then return end
  local happy, msg = req_sock:poll()
  if not happy then
    req_sock:close() -- good form
    req_sock = nil
    return
  end
  if msg then
    table.insert(parts, msg)
    print(part_order[#parts], parts[#parts])
  end
end)

-- You can optionally ask for only the body:
local req_sock = pollnet.http_get("https://www.example.com", true)
local part_order = {"STATUS CODE:", "HEADERS:", "BODY:"}
local parts = {}
each_game_tick(function()
  if not req_sock then return end
  local happy, msg = req_sock:poll()
  if not happy then
    req_sock:close() -- good form
    req_sock = nil
    return
  end
  if msg then
    table.insert(parts, msg)
    print(part_order[#parts], parts[#parts])
  end
end)
```

# FAQ

## Why would I want this?

This is most useful for running from within the LuaJIT environment of a game, or
other interactive-type application, where you don't have control over the overall
program flow, but only get to run some Lua every frame or simulation 'tick'.

## What makes this useful for mods?

- simple to build, complete with TLS/WSS support, even on Windows
- broad compatibility with LuaJIT binaries: no worrying about which compiler LuaJIT was built with
- compact C API (<20 functions) using only basic types
- speaks websockets and secure websockets out-of-the-box

## Can this be used outside of LuaJIT?

It can be used from anything that can link a C-API dynamic library (so in practice, anything
with an FFI).

# Building
Pollnet builds with Rust in the standard way. Assuming you're on a 64 bit machine,
building for 64 bit:
```
cargo build --release
```

However to use this from a 32 bit Windows binary (e.g., LuaJIT in a 32 bit application such as Noita, MWSE, etc.),
you'll need to build for 32 bit:
```
rustup target add i686-pc-windows-msvc
cargo build --target=i686-pc-windows-msvc --release
```

The resulting .dll will end up in `target/i686-pc-windows-msvc/release/pollnet.dll`.
