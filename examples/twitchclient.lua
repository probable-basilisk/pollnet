local pollnet = require("pollnet")

if #arg < 1 then
  error("Usage: luajit twitchclient.lua CHANNELNAME")
end

local target_channel = arg[1]
local url = "wss://irc-ws.chat.twitch.tv:443"

local thread = coroutine.create(function()
  local sock = pollnet.open_ws(url)
  -- Note: connecting is asynchronous, so the
  -- socket isn't yet open! But we can queue up sends anyway.

  -- special nick for anon read-only access on twitch
  local anon_user_name = "justinfan" .. math.random(1, 100000)
  sock:send("NICK " .. anon_user_name)
  sock:send("JOIN #" .. target_channel)

  while true do
    local happy, msg = sock:poll()
    if not happy then break end
    if msg and msg == "PING :tmi.twitch.tv" then
      sock:send("PONG :tmi.twitch.tv")
    elseif msg then
      print(msg)
    end
    coroutine.yield()
  end
  print("Socket closed: ", sock:last_message())
end)

-- In a typical application-embedded Lua context the application would
-- have its own 'main loop', which we emulate here
while coroutine.status(thread) ~= "dead" do
  coroutine.resume(thread)
  pollnet.sleep_ms(20) -- Blocking!
end
