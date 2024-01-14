local pollnet = require("pollnet")

if #arg < 1 then
  error("Usage: luajit twitchclient.lua CHANNELNAME")
end

local target_channel = arg[1]
local url = "wss://irc-ws.chat.twitch.tv:443"

local reactor = pollnet.Reactor()
reactor:run(function()
  local sock = pollnet.open_ws(url)
  -- Note: connecting is asynchronous, so the
  -- socket isn't yet open! But we can queue up sends anyway.

  -- special nick for anon read-only access on twitch
  local anon_user_name = "justinfan" .. math.random(1, 100000)
  sock:send("NICK " .. anon_user_name)
  sock:send("JOIN #" .. target_channel)

  while true do
    local msg = assert(sock:await())
    if msg == "PING :tmi.twitch.tv" then
      sock:send("PONG :tmi.twitch.tv")
    else
      print(msg)
    end
  end
end)

-- In a typical application-embedded Lua context the application would
-- have its own 'main loop', and you'd just call reactor:update once per frame
while reactor:update() > 0 do
  pollnet.sleep_ms(50) -- Blocking!
end
