local pollnet = require("pollnet")

if #arg < 1 then
  error("Usage: luajit twitchclient.lua CHANNELNAME")
end

local target_channel = arg[1]
local url = "wss://irc-ws.chat.twitch.tv:443"

local sock = pollnet.open_ws(url)
-- Waiting for open isn't strictly necessary
while sock:status() ~= "open" and sock:poll() do
  print("Connecting...")
  pollnet.sleep_ms(200)
end
print("Connected!")

sock:send("PASS doesntmatter")
-- special nick for anon read-only access on twitch
local anon_user_name = "justinfan" .. math.random(1, 100000)
sock:send("NICK " .. anon_user_name)
sock:send("JOIN #" .. target_channel)

while sock:poll() do
  local msg = sock:last_message()
  if msg then
    if msg == "PING :tmi.twitch.tv" then
      sock:send("PONG :tmi.twitch.tv")
    end
    print(msg)
  else
    pollnet.sleep_ms(20)
  end
end
print("Socket closed: ", sock:last_message())