local pollnet = require("pollnet")

print("Pollnet version:", pollnet.VERSION)
print("Starting up test WS+HTTP+TCP servers")

local threads = {}
local new_threads = {}

local function add_thread(name, threadfunc)
  assert(not threads[name], "Thread " .. name .. " already exists.")
  new_threads[name] = coroutine.create(threadfunc)
end

local function merge_new()
  for name, thread in pairs(new_threads) do
    assert(not threads[name])
    threads[name] = thread
  end
  new_threads = {}
end

local function mainloop()
  while true do
    for name, thread in pairs(threads) do
      local happy, msg = coroutine.resume(thread)
      if not happy then
        print(("Thread %s ended: %s"):format(name, msg or "?"))
        threads[name] = nil
      end
    end
    merge_new()
    pollnet.sleep_ms(20)
  end
end

local function pollsock(name, sock, handler)
  handler = handler or print
  while sock:poll() do
    if sock:last_message() then
      handler(name, sock, sock:last_message())
    end
    coroutine.yield()
  end
  sock:close()
  print(("Sock %s closed: %s"):format(name, sock:last_message() or "?"))
end

local function echo_handler(name, sock, msg)
  print("Msg from", name, msg)
  sock:send("ECHO:" .. msg)
end

local function countdown_handler(name, sock, msg)
  print("Msg from", name, msg)
  local count = tonumber(msg)
  if not count then
    sock:send("ECHO:" .. msg)
    return
  end
  for idx = 1, count do
    sock:send("COUNT: " .. idx)
    coroutine.yield()
  end
  sock:close()
end

local function client_handler(prefix, inner_handler)
  inner_handler = inner_handler or echo_handler
  return function (sock, addr)
    local name = ("%s:%s"):format(prefix, addr)
    print("Got client?", name)
    add_thread(name, function()
      pollsock(name, sock, inner_handler)
    end)
  end
end

add_thread("ws_server", function()
  local ws_server_sock = pollnet.listen_ws("0.0.0.0:9090")
  ws_server_sock:on_connection(client_handler("WS", countdown_handler))
  pollsock("WS_SERVER", ws_server_sock)
end)

add_thread("ws_server_ipv6", function()
  local ws_server_sock = pollnet.listen_ws("[::]:9090")
  ws_server_sock:on_connection(client_handler("WS_IPV6"))
  pollsock("WS_SERVER_IPV6", ws_server_sock)
end)

add_thread("tcp_server", function()
  local tcp_server_sock = pollnet.listen_tcp("0.0.0.0:6000")
  tcp_server_sock:on_connection(client_handler("TCP"))
  pollsock("TCP_SERVER", tcp_server_sock)
end)

add_thread("http_server", function()
  local http_server_sock = pollnet.serve_http("0.0.0.0:8080", "test_www_dir", SCRATCH)
  http_server_sock:add_virtual_file("virt/a.txt", "HELLO_VIRTUAL")
  http_server_sock:add_virtual_file("virt/b.bin", "HELLO\x00\x00VIRTUAL\x00")
  pollsock("HTTP_SERVER", http_server_sock)
end)

mainloop()