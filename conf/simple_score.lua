-- A simple demo for using Lua script to customize proxy scoring.
-- Run moproxy with `--score-script /path/to/simple_score.lua` to enable it.

-- Calculate score for given proxy server and delay
-- proxy: a table describes the proxy server
-- delay: time in seconds in float
-- Return a score in signed number or nil
function calc_score(proxy, delay)
  -- proxy.addr, proxy.proto, proxy.tag:
  --   Basic information about the proxy.
  -- proxy.config:
  --   Proxy's configs, 
  --   includes test_dns, max_wait, and score_base.
  -- proxy.traffic:
  --   tx_bytes: total amount of traffics, upload to proxy server
  --   rx_bytes: download from proxy server
  -- proxy.status:
  --   delay: the delay before this update, in secs in float.
  --          nil = initial value; -1 = timed out.
  --   score: the score before this update, may be nil.
  --   conn_alive, conn_total, conn_error: connection counters
  --   close_history:
  --     History of the 64 most recent closed connections, stored as
  --     bitmap in a 64-bit int. 0 for closed without any error, 1 for
  --     connection closed due to error. The most insignificant bit is
  --     the most recent closed connection.

  -- print out tag & delay for debugging
  print(proxy.tag, delay)

  if delay == nil then
    -- disable proxy if delay probing failed
    return nil
  else
    -- simply use delay in microseconds plus score_base as score
    return math.floor(delay * 1000 + proxy.config.score_base)
  end
end
