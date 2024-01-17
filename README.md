# pollnet
Compact polling-based C-API networking library designed to be easily used from
within game-embedded LuaJIT environments (e.g., mods)

# Features
* Websocket client and server (both ws:// and wss:// for clients)
* TCP client and server
* bare-bones HTTP client: simple GET/POST
* simple HTTP server: serve static files from disk or from memory
* dynamic HTTP server: construct HTTP responses to requests

# Usage (luajit FFI bindings)
```Lua
-- pollnet.lua FFI loads pollnet.dll
local pollnet = require("pollnet")

-- Reactor is a convenience for running Lua coroutines
local reactor = pollnet.Reactor()

reactor:run(function()
  local sock = pollnet.open_ws("wss://irc-ws.chat.twitch.tv:443")
  -- special nick for anon read-only access on twitch
  sock:send("NICK justinfan" .. math.random(1, 100000))
  sock:send("JOIN #some_twitch_channel")

  while true do
    print("CHAT: ", sock:await())
  end
end)

reactor:run(function()
  -- HTTP requests return three messages in order:
  -- status code, headers, body
  local req_sock = pollnet.http_get("https://www.example.com")
  print("Status:", req_sock:await())
  print("Headers:", req_sock:await())
  print("Body:", req_sock:await())
end)

-- This part will be application specific:
-- you need to call reactor:update() regularly (e.g., every frame)
function on_frame()
  reactor:update()
end
AppSpecifiAPI.setFrameCallback(on_frame)

```

# FAQ

## Why would I want this?

This is most useful for running from within the LuaJIT environment of a game, or
other interactive-type application, where you don't have control over the overall
program flow, but only get to run some Lua every frame or simulation 'tick'.

## What makes this useful for mods?

- nonblocking and callback free
- simple to build, complete with TLS/WSS support, even on Windows
- broad compatibility with LuaJIT binaries: no worrying about which compiler LuaJIT was built with
- straightforward C API for easy use from FFIs

## Can this be used outside of LuaJIT?

It can be used from anything that can link a C-API dynamic library (so in practice, anything
with an FFI).

# Building
Prebuilt binaries (including win32) are available from releases.

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
