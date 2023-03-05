local pollnet = require("pollnet")

if #arg < 1 then
  error("Usage: luajit httpget.lua URL [outfile]")
end
local url = arg[1]
local outfn = arg[2]

local function poll_blocking(sock)
  while sock:poll() do
    if sock:last_message() then
      return sock:last_message()
    end
    pollnet.sleep_ms(20)
  end
  error("Socket closed?", sock:last_message())
  sock:close()
end

print("GET", url)
local sock = pollnet.http_get(url, false)
local status = poll_blocking(sock)
print("HTTP Status:", status)
local headers = poll_blocking(sock)
print("HTTP Headers:")
print(headers)
local body = poll_blocking(sock)
if outfn then
  local outfile = io.open(outfn, "wb")
  outfile:write(body)
  outfile:close()
  print("Wrote", #body, "bytes to", outfn)
else
  print("Body:")
  print(body)
end
sock:close()