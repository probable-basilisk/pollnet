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

local function tag(name)
  return function(children)
    return ("<%s>%s</%s>"):format(name, table.concat(children), name)
  end
end

local counts = {}

local handler = pollnet.wrap_req_handler(function(req)
  print("METHOD:", req.method)
  print("PATH:", req.path)
  print("-- HEADERS --")
  local header_items = {}
  counts[req.path] = (counts[req.path] or 0) + 1
  for k, v in pairs(req.headers) do
    print(k, "->", v)
    table.insert(header_items, tag"li"{k, ": ", v})
  end
  if req.path == "/favicon.ico" then return FAVICON end
  local body = tag"html"{tag"body"{
    tag"p"{req.method, " ", req.path},
    tag"p"{"This path has been requested ", counts[req.path], " times"},
    tag"ul"(header_items)
  }}
  return {
    status = "200",
    headers = {
      ['Set-Cookie'] = {"session=1", "foobar=hello"}
    },
    body = body
  }
end, true)

local reactor = pollnet.Reactor()
local HOST_ADDR = "0.0.0.0:8080"
reactor:run_server(pollnet.serve_dynamic_http(HOST_ADDR, true), handler)

print("Serving on", HOST_ADDR)

while reactor:update() > 0 do
  pollnet.sleep_ms(50)
end
