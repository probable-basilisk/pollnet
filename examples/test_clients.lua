local pollnet = require("pollnet")

local DELAY_MS = 20

local function poll_messages(sock, count, timeout)
  count = count or 1
  timeout = timeout or 1000
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
      error("Socket timed out.")
    end
    timeout = timeout - DELAY_MS
    pollnet.sleep_ms(DELAY_MS)
  end
  error("Socket closed", sock:last_message())
end

local function test_local_ws()
  local sock = pollnet.open_ws("ws://127.0.0.1:9090")
  pollnet.sleep_ms(100)
  sock:send("HELLO")
  local res = poll_messages(sock, 1)
  assert(res[1] == "ECHO:HELLO")
  print("WS: OK")
end

local function test_local_tcp()
  local sock = pollnet.open_tcp("127.0.0.1:6000")
  pollnet.sleep_ms(100)
  sock:send("HELLO")
  local res = poll_messages(sock, 1)
  assert(res[1] == "ECHO:HELLO")
  print("TCP: OK")
end

local function test_local_http()
  local sock = pollnet.http_get("http://127.0.0.1:8080/testfile.txt", false)
  local res = poll_messages(sock, 3) -- status, headers, body
  assert(res[1] == 200)
  assert(res[3] == "TEST1234")
  print("HTTP GET: OK")
end

test_local_ws()
test_local_tcp()
--test_local_http()