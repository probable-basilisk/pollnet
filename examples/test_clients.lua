local pollnet = require("pollnet")

local DELAY_MS = 100
local OPEN_DELAY_MS = 0
local TIMEOUT = 2000

local function poll_messages(sock, count, timeout)
  count = count or 1
  timeout = timeout or TIMEOUT
  local msgs = {}
  while sock:poll() do
    if sock:last_message() then
      print("MSG:", sock:last_message())
      table.insert(msgs, sock:last_message())
      if #msgs == count then
        sock:close()
        return msgs
      end
    end
    if timeout <= 0 then
      print("Socket timed out.")
      sock:close()
      return {}, "timeout"
    end
    timeout = timeout - DELAY_MS
    pollnet.sleep_ms(DELAY_MS)
  end
  local errmsg = sock:last_message()
  print("Socket closed", errmsg)
  sock:close()
  return {}, errmsg
end

local function sync_sleep(ms)
  if ms > 0 then
    pollnet.sleep_ms(ms)
  end
end

local test_successes = 0
local test_failures = 0

local function test_outcome(succeeded, testname, failure_detail)
  if succeeded then
    test_successes = test_successes + 1
    print(testname, ": [OK]")
  else
    test_failures = test_failures + 1
    print(testname, ": [FAIL]")
  end
end

local function expect(val, expected, msg)
  test_outcome(val == expected, msg, 
    ("Expected [%s], got [%s]"):format(tostring(val), tostring(expected))
  )
end

local function expect_match(val, patt, msg)
  test_outcome(val and val:match(patt), msg, 
    ("[%s] does not match pattern [%s]"):format(tostring(val), patt)
  )
end

local function test_local_ws()
  local sock = pollnet.open_ws("ws://127.0.0.1:9090")
  sync_sleep(OPEN_DELAY_MS)
  sock:send("HELLO")
  local res = poll_messages(sock, 1)
  expect(res[1], "ECHO:HELLO", "WS round trip")
end

local function test_local_tcp()
  local sock = pollnet.open_tcp("127.0.0.1:6000")
  sync_sleep(OPEN_DELAY_MS)
  sock:send("HELLO")
  local res = poll_messages(sock, 1)
  expect(res[1], "ECHO:HELLO", "TCP round trip")
end

local function test_local_http()
  local sock = pollnet.http_get("http://127.0.0.1:8080/testfile.txt", false)
  local res = poll_messages(sock, 3) -- status, headers, body
  expect_match(res[1], "^200", "HTTP GET status 200")
  expect(res[3], "TEST1234", "HTTP GET body")

  local sock = pollnet.http_get("http://127.0.0.1:8080/idontexist.txt", false)
  local res = poll_messages(sock, 3) -- status, headers, body
  expect_match(res[1], "^404", "HTTP GET status 404")

  local sock = pollnet.http_get("http://127.0.0.1:8080/virt/a.txt", true)
  local res = poll_messages(sock, 1)
  expect(res[1], "HELLO_VIRTUAL", "HTTP GET virtual + body only")

  local sock = pollnet.http_get("http://127.0.0.1:8080/virt/b.bin", false)
  local res = poll_messages(sock, 3)
  expect(res[3], "HELLO\x00\x00VIRTUAL\x00", "HTTP GET binary")

  -- no server should be open on this socket
  --[[
  local sock = pollnet.http_get("http://127.0.0.1:9999/virt/b.bin", false)
  local res, errmsg = poll_messages(sock, 3)
  expect(#res, 0, "HTTP Refused")
  expect_match(errmsg, "tcp connect error", "HTTP Refused Error Message")
  ]]
end

test_local_ws()
test_local_tcp()
test_local_http()

print("--------- Test Results ---------")
print("Succeeded:", test_successes)
print("Failed:", test_failures)

if test_failures > 0 then
  os.exit(1)
end