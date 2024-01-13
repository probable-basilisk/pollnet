local pollnet = require("pollnet")

print("Pollnet version:", pollnet.VERSION)
print("Starting up test WS+HTTP+TCP servers")

local reactor = pollnet.Reactor()

local function echo_thread(sock, name)
  while true do
    local msg = assert(sock:await())
    print("Msg from", name, msg)
    sock:send("ECHO:" .. msg)
  end
end

local function countdown_thread(sock, name)
  local msg = sock:await()
  print("Msg from", name, msg)
  local count, yield = msg:match("^([%d]+) ?(.*)$")
  count = tonumber(count)
  if not count then
    sock:send("ECHO:" .. msg)
    sock:close()
    return
  end
  for idx = 1, count do
    sock:send("COUNT: " .. idx)
    if yield ~= "BLAST" then coroutine.yield() end
  end
  sock:close()
end

local function client_handler(prefix, inner_handler)
  inner_handler = inner_handler or echo_thread
  return function(sock, addr)
    local name = ("%s:%s"):format(prefix, addr)
    print("Got client?", name)
    reactor:run(function()
      inner_handler(sock, name)
    end)
  end
end

reactor:run_server(
  pollnet.listen_ws("0.0.0.0:9090"),
  client_handler("WS", countdown_thread)
)

do
  local port = 9696
  if jit.os:lower() == "windows" then
    -- windows will let us bind the same port in both ipv4 and ipv6; 
    -- (Linux does not)
    port = 9090
  end
  reactor:run_server(
    pollnet.listen_ws("[::]:" .. port),
    client_handler("WS_IPV6")
  )
end

reactor:run_server(
  pollnet.listen_tcp("0.0.0.0:6000"),
  client_handler("TCP")
)

do
  local http_server_sock = pollnet.serve_http("0.0.0.0:8080", "test_www_dir", SCRATCH)
  http_server_sock:add_virtual_file("virt/a.txt", "HELLO_VIRTUAL")
  http_server_sock:add_virtual_file("virt/b.bin", "HELLO\x00\x00VIRTUAL\x00")
  reactor:run(function()
    while true do
      local msg = http_server_sock:await()
      error("Somehow got an HTTP server message! " .. msg)
    end
  end)
end

while reactor:update() > 0 do
  pollnet.sleep_ms(20)
end
