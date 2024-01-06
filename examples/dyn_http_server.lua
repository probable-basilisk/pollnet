local pollnet = require("pollnet")

local threads = {}
local function update_all()
  for name, thread in pairs(threads) do
    local happy, msg = coroutine.resume(thread)
    if not happy then
      print(name, "died, reason:", msg)
      threads[name] = nil
    end
  end
end

local function async_poll(sock)
  while sock:poll() do
    if sock:last_message() then
      return sock:last_message()
    end
    coroutine.yield()
  end
  error("Socket died!")
end

local function handle_req(req)
  print("METHOD:", req.method)
  print("HEADERS:", req.headers)
  print("BODY:", req.body)
  return {
    status = "200",
    headers = {},
    body = "Wow"
  }
end

local HOST_ADDR = "0.0.0.0:8080"

local sock = pollnet.serve_dynamic_http(HOST_ADDR, function(req_sock, addr)
  print("Incoming request from", addr)
  threads[addr] = coroutine.create(function()
    local method_path = async_poll(req_sock)
    local headers = async_poll(req_sock)
    local body = async_poll(req_sock)
    local reply = handle_req{
      method = method_path, 
      headers = headers, 
      body = body
    }
    print("Sending reply?")
    req_sock:send(reply.status or "404")
    req_sock:send(pollnet.format_headers(reply.headers or {}))
    req_sock:send_binary(reply.body or "")
    req_sock:close()
    print("Sent reply?")
  end)
end)

print("Serving on", HOST_ADDR)

while sock:poll() do
  update_all()
  pollnet.sleep_ms(100)
end
