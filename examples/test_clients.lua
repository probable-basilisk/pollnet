local pollnet = require("pollnet")
print("Pollnet version:", pollnet.VERSION)

local DELAY_MS = 100
local STARTUP_DELAY_MS = 500
local OPEN_DELAY_MS = 0
local TIMEOUT = 2000

local test_successes = 0
local test_failures = 0

local function ok(succeeded, testname, failure_detail)
  if succeeded then
    test_successes = test_successes + 1
    print("[ OK ]: " .. testname)
  else
    test_failures = test_failures + 1
    print("[FAIL]: " .. testname .. " => " .. (failure_detail or ""))
  end
end

local function expect(val, expected, msg)
  ok(val == expected, msg, 
    ("Expected [%s], got [%s]"):format(tostring(expected), tostring(val))
  )
end

local function expect_size(val, esize, msg)
  local vsize = val and #val
  ok(vsize and vsize >= esize, msg, 
    ("Expected %d bytes of content; got %s"):format(esize, tostring(vsize))
  )
end

local function expect_match(val, patt, msg)
  ok(val and val:match(patt), msg, 
    ("[%s] does not match pattern [%s]"):format(tostring(val), patt)
  )
end

local function async_sleep(ms)
  if ms <= 0 then return end
  local count = math.ceil(ms / DELAY_MS)
  for idx = 1, count do
    coroutine.yield()
  end
end

local function get_messages_until_closed(sock)
  local messages = {}
  sock.timeout = TIMEOUT / DELAY_MS
  while true do
    local msg = sock:await()
    if not msg then break end
    table.insert(messages, msg)
  end
  return messages
end

local function with_timeout(sock)
  sock.timeout = TIMEOUT / DELAY_MS
  return sock
end

local function test_local_ws()
  local sock = with_timeout(pollnet.open_ws("ws://127.0.0.1:9090"))
  async_sleep(OPEN_DELAY_MS)
  sock:send("HELLO")
  local res = assert(sock:await())
  expect(res, "ECHO:HELLO", "WS round trip")

  -- test IPV6 websocket
  -- in windows we can bind the same port for both v4 and v6,
  -- so test that that works
  local v6port = 9696
  if jit.os:lower() == "windows" then
    v6port = 9090
  end
  local sock = with_timeout(pollnet.open_ws("ws://[::1]:" .. v6port))
  async_sleep(OPEN_DELAY_MS)
  sock:send("HELLO")
  local res = assert(sock:await())
  expect(res, "ECHO:HELLO", "WS (IPV6) round trip")

  -- test that we get exactly the expected number of messages
  local sock = with_timeout(pollnet.open_ws("ws://127.0.0.1:9090"))
  sock:send(tostring(13))
  local res = assert(get_messages_until_closed(sock))
  expect(#res, 13, "WS correct message count")

  -- test getting messages faster than yield rate
  local t0 = os.time()
  local sock = with_timeout(pollnet.open_ws("ws://127.0.0.1:9090"))
  -- blast enough messages that it would take 5s to receive them if we
  -- only polled one message per DELAY_MS
  local count = math.ceil(5000 / DELAY_MS) 
  sock:send(("%d BLAST"):format(count))
  local res = assert(get_messages_until_closed(sock))
  local dt = os.time() - t0
  expect(#res, count, "WS BLAST correct message count")
  ok(dt < 1, "WS BLAST received faster than yield rate", ("Took %f"):format(dt))
end

local function validate_status_transitions(statuses, testname)
  local ALLOWED = {
    ["unpolled->*"] = true,
    ["*->error"] = true,
    ["opening->open"] = true,
    ["open->closed"] = true,
  }
  local function tr(a, b) return a .. "->" .. b end
  for idx = 2, #statuses do
    local a = statuses[idx-1]
    local b = statuses[idx]
    if a == b or ALLOWED[tr(a, b)] or ALLOWED[tr(a, "*")] or ALLOWED[tr("*", b)] then
      -- transition was OK
    else
      ok(false, testname, "invalid transition: " .. tr(a, b))
      return false
    end
  end
  ok(true, testname)
end

local function test_ws_status_flow()
  local sock = pollnet.open_ws("ws://127.0.0.1:9090")
  sock:send("HELLO")
  local statuses = {}
  for iter = 1, 10 do
    local happy = sock:poll()
    table.insert(statuses, sock:status())
    if not happy then break end
    coroutine.yield()
  end
  sock:close()
  validate_status_transitions(statuses, "WS status flow")
end

local function test_local_tcp()
  local sock = with_timeout(pollnet.open_tcp("127.0.0.1:6000"))
  async_sleep(OPEN_DELAY_MS)
  sock:send("HELLO")
  local res = sock:await()
  expect(res, "ECHO:HELLO", "TCP round trip")
end

local function http_get(url, headers, body_only)
  local sock = with_timeout(pollnet.http_get(url, headers, body_only))
  local n_parts = 3 -- status, headers, body
  if body_only then n_parts = 1 end
  return assert(sock:await_n(n_parts))
end

local function http_post(url, headers, body)
  local sock = with_timeout(pollnet.http_post(url, headers, body, false))
  return assert(sock:await_n(3))
end

local function test_local_http()
  local res = http_get("http://127.0.0.1:8080/testfile.txt")
  expect_match(res[1], "^200", "HTTP GET status 200")
  expect(res[3], "TEST1234", "HTTP GET body")

  local res = http_get("http://127.0.0.1:8080/foo/bar/../../testfile.txt")
  expect_match(res[1], "^200", "HTTP GET .. traversal status 200")
  expect(res[3], "TEST1234", "HTTP GET .. traversal body")

  local res = http_get("http://127.0.0.1:8080/../test_clients.lua")
  expect_match(res[1], "^404", "HTTP GET path traversal attack fails (404)")

  local res = http_get("http://127.0.0.1:8080/idontexist.txt")
  expect_match(res[1], "^404", "HTTP GET status 404")

  local res = http_get("http://127.0.0.1:8080/virt/a.txt", nil, true)
  expect(res[1], "HELLO_VIRTUAL", "HTTP GET virtual + body only")

  local res = http_get("http://127.0.0.1:8080/virt/b.bin")
  expect(res[3], "HELLO\x00\x00VIRTUAL\x00", "HTTP GET binary")

  local res = http_get("http://127.0.0.1:8383/foo/bar.txt")
  expect_match(res[1], "^404", "Dyn HTTP GET missing file correct 404")

  local postbody = "HELLO\x00\x00dynamic\x00"
  local res = http_post("http://127.0.0.1:8383/foo/bar.txt", {}, postbody)
  expect_match(res[1], "^200", "HTTP POST succeeded")

  local res = http_get("http://127.0.0.1:8383/foo/bar.txt")
  expect_match(res[1], "^200", "Dyn HTTP GET succeeded")
  expect(res[3], postbody, "Dyn HTTP GET body")

  -- no server should be open on this socket
  --[[
  local sock = pollnet.http_get("http://127.0.0.1:9999/virt/b.bin", false)
  local res, errmsg = sync_get_messages(sock, 3)
  expect(#res, 0, "HTTP Refused")
  expect_match(errmsg, "tcp connect error", "HTTP Refused Error Message")
  ]]
end

local function test_https()
  local res = http_get("https://example.com/")
  expect_match(res[1], "^200", "HTTPS GET status 200")
  expect_size(res[3], 500, "HTTPS GET body has content")
end

local function poll_until_open(sock)
  while true do
    sock:poll()
    local status = sock:status()
    if status == "open" then 
      return true 
    elseif status == "error" or status == "closed" then
      return false
    end
    coroutine.yield()
  end
end

local function connect_ws_with_retries(url, max_retries, retry_delay)
  max_retries = max_retries or 5
  retry_delay = retry_delay or 1000
  for idx = 1, max_retries do
    local sock = pollnet.open_ws(url)
    if poll_until_open(sock) then
      return sock
    else
      print("Failed connection attempt " .. idx)
      sock:close()
      async_sleep(retry_delay)
    end
  end
  return nil
end

local function test_wss()
  -- since echo.websocket.org is gone, twitch is about the most
  -- convenient secure websocket host to test against
  -- special nick for anon read-only access on twitch
  local anon_user_name = "justinfan" .. math.random(1, 100000)
  local sock = connect_ws_with_retries("wss://irc-ws.chat.twitch.tv:443")
  if not sock then
    ok(false, "WSS got something back", "Connection failure")
  end
  --sock:send("PASS doesntmatter")
  sock:send("NICK " .. anon_user_name)
  local res = assert(sock:await())
  print(res)
  expect_size(res, 10, "WSS got something back")
end

local reactor = pollnet.Reactor()
reactor:run(function()
  async_sleep(STARTUP_DELAY_MS)

  test_local_ws()
  test_ws_status_flow()
  test_local_tcp()
  test_local_http()
  test_https()
  test_wss()

  print("--------- Test Results ---------")
  print("Succeeded:", test_successes)
  print("Failed:", test_failures)
end)

while reactor:update() > 0 do
  pollnet.sleep_ms(DELAY_MS)
end

if test_failures > 0 then
  os.exit(1)
end