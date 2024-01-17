local pollnet = require("pollnet")

local FAVICON = {
  status = "200",
  headers = {
    ['content-type'] = "image/svg+xml"
  },
  -- Note! SVG documents must have *no whitespace* at front!
  body = [[<?xml version="1.0" standalone="no"?>
    <svg viewBox="0 0 200 200" xmlns="http://www.w3.org/2000/svg">
    <path fill="#FF0066" d="M47.3,-48.8C61.8,-32.9,74.4,-16.4,68.5,-5.8C62.7,4.8,38.5,9.5,24,17.4C9.5,25.3,4.8,36.3,-4.3,40.6C-13.3,44.8,-26.5,42.3,-32.6,34.4C-38.6,26.5,-37.5,13.3,-35.9,1.6C-34.3,-10.1,-32.3,-20.2,-26.3,-36.1C-20.2,-52,-10.1,-73.7,3.2,-76.8C16.4,-80,32.9,-64.6,47.3,-48.8Z" transform="translate(100 100)" />
    </svg>]]
}

local function handle_req(method_path, headers, body)
  local method, path, queries = pollnet.parse_method(method_path)
  print("METHOD:", method)
  print("PATH:", path)
  print("QUERIES:", queries[1])
  print("HEADERS:", headers)
  print("BODY:", body)

  headers = pollnet.parse_headers(headers)
  print("----")
  for k, v in pairs(headers) do
    print(k, "->", v)
  end
  if path == "/favicon.ico" then return FAVICON end
  return {
    status = "200",
    headers = {
      ['Set-Cookie'] = {"session=1", "foobar=hello"}
    },
    body = "Wow"
  }
end

local reactor = pollnet.Reactor()
local HOST_ADDR = "0.0.0.0:8080"
reactor:run_server(pollnet.serve_dynamic_http(HOST_ADDR, true), function(req_sock, addr)
  print("New connection from:", addr)
  while true do
    local req = req_sock:await_n(3)
    if not req then break end
    print("Request from", addr)
    local reply = handle_req(unpack(req))
    req_sock:send(reply.status or "404")
    req_sock:send(pollnet.format_headers(reply.headers or {}))
    req_sock:send_binary(reply.body or "")
  end
  print("Connection", addr, "closed.")
  req_sock:close()
end)

print("Serving on", HOST_ADDR)

while reactor:update() > 0 do
  pollnet.sleep_ms(50)
end
