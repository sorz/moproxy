# moproxy

A transparent TCP to SOCKSv5/HTTP proxy on *Linux* written in Rust.

Features:

 * Transparent TCP proxy with `iptables -j REDIRECT` or `nft redirect to`
 * Downstream SOCKSv5 as a supplement to transparent proxy
 * Multiple SOCKSv5/HTTP upstream proxy servers
 * SOCKS/HTTP-layer alive & latency probe for upstreams
 * Prioritize upstreams according to connection quality (latency & error rate)
 * Full IPv6 support
 * Proxy selection policy (see [conf/policy.rules](conf/policy.rules))
 * Multiple downstream listen ports (for proxy selection policy)
 * Remote DNS resolving for TLS with SNI (extract domain name from TLS
   handshaking)
 * Optional try-in-parallel for TLS (try multiple proxies and choose the one
   first response)
 * Optional status web page (latency, traffic, etc. w/ curl-friendly output)
 * Optional [Graphite](https://graphite.readthedocs.io/) and
   OpenMetrics ([Prometheus](https://prometheus.io/)) support
   (to build fancy dashboard with [Grafana](https://grafana.com/) for example)
 * Customizable proxy selection algorithm with Lua script (see
   [conf/simple_scroe.lua](conf/simple_score.lua)).

```
+-----+  TCP  +-----------+       SOCKSv5   +---------+
| App |------>| firewall  |    +----------->| Proxy 1 |--->
+-----+       +-----------+    |            +---------+
            redirect |         |
+-----+           to v         |      HTTP  +---------+
| App |       //=========\\    |   +------->| Proxy 2 |--->
+-----+       ||         ||----+   |        +---------+
   |          || MOPROXY ||--------+             :
   +--------->||         ||-----------···        :
   SOCKSv5    \\=========//  Selection  |   +---------+
                          |  policy     +-->| Proxy N |--->
                          |                 +---------+
                          |
                          +----------- Direct ------------>
```

## Breaking changes

There are CLI and/or configure changes among:

- [v0.4 => v0.5](MIGRATION.md/#v04-to-v05) 
- [v0.3 => v0.4](MIGRATION.md/#v03-to-v04) 

See [MIGRATION.md](MIGRATION.md)

## Usage

### Print usage
```bash
moproxy --help
```
### Examples

Assume there are three SOCKSv5 servers on `localhost:2001`, `localhost:2002`,
and `localhost:2003`, and two HTTP proxy servers listen on `localhost:3128`
and `192.0.2.0:3128`.
Following commands forward all TCP connections that connect to 80 and 443 to
these proxy servers.

```bash
moproxy --port 2080 --socks5 2001 2002 2003 --http 3128 192.0.2.0:3128

# redirect local-initiated connections
nft add rule nat output tcp dport {80, 443} redirect to 2080
# redirect connections initiated by other hosts (if you are router)
nft add rule nat prerouting tcp dport {80, 443} redirect to 2080

# or the legacy iptables equivalent
iptables -t nat -A OUTPUT -p tcp -m multiport --dports 80,443 -j REDIRECT --to-port 2080
iptables -t nat -A PREROUTING -p tcp -m multiport --dports 80,443 -j REDIRECT --to-port 2080
```

SOCKSv5 server is also launched alongs with transparent proxy on the same port:
```bash
http_proxy=socks5h://localhost:2080 curl ifconfig.co
```

### Server list file
Put upstream proxies on a file to avoid messy CLI arguments and enable features
like priority (score base), username/password auth, capabilities, etc.

[See proxy.ini example](conf/proxy.ini) for details.

Pass file path to `moproxy` via `--list` argument.

Signal `SIGHUP` will trigger the program to reload the list.

### Proxy selection policy file
Let specified connections use only a subset of upstream proxies.

[See policy.rules example](conf/policy.rules) for details.

Pass file path to `moproxy` via `--policy` argument.

Signal `SIGHUP` will trigger the program to reload the list.

### Custom proxy selection
Proxy servers are sorted by their *score*, which is re-calculated after each
round of alive/latency probing. Server with lower score is prioritized.

The current scoring algorithm is a kind of weighted moving average of latency
with penalty for recent connection errors. This can be replaced with your own
algorithm written in Lua. See [conf/simple_score.lua](conf/simple_score.lua)
for details.

Source/destination address–based proxy selection is not directly supported.
One workaround is let moproxy bind multiple ports, delegates each port to
different proxy servers with `listen ports` in your config, then doing
address-based selection on your firewall.

### Monitoring
Metrics (latency, traffic, number of connections, etc.) are useful for
diagnosis and customing your own proxy selection. You can access these
metrics with various methods, from a simple web page, curl, to specialized
tools like Graphite or Prometheus.

`--stats-bind [::1]:8080` turns on the internal stats page, via HTTP, on the
given IP address and port number. It returns a HTML page for web browser,
or a ASCII table for `curl`.

The stats page only provides current metrics and a few aggregations. Graphite
(via `--graphite`) or OpenMetrics (via `--stats-bind` then `\metrics`) should
be used if you want a full history.

Some examples of Prometheus query (Grafana variant):

```
Inbound bandwith:
rate(moproxy_proxy_server_bytes_rx_total[$__range])

Total outbound traffic:
sum(increase(moproxy_proxy_server_bytes_tx_total[$__range]))

No. of connection errors per minute:
sum(increase(moproxy_proxy_server_connections_error[1m]))

Average delay for each proxy server:
avg_over_time(moproxy_proxy_server_dns_delay_seconds[$__interval])
```

### Systemd integration

Sample service file: [conf/moproxy.service](conf/moproxy.service)

Implemented features:

- Watchdog
- Reloading (via SIGHUP signal)
- Notify (`type=notify`, reloading, status string)

Get simple status without turing on the HTTP stats page:

```
$ systemctl status moproxy
> ...
> Status: "serving (7/11 upstream proxies up)"
> ...
```

## Install

You may download the binary executable file on
[releases page](https://github.com/sorz/moproxy/releases).

Arch Linux user can install it from
[AUR/moproxy](https://aur.archlinux.org/packages/moproxy/).

Or compile it manually:

```bash
# Install Rust
curl https://sh.rustup.rs -sSf | sh

# Clone source code
git clone https://github.com/sorz/moproxy
cd moproxy

# Build
cargo build --release
target/release/moproxy --help

# If you are in Debian
cargo install cargo-deb
cargo deb
sudo dpkg -i target/debian/*.deb
moproxy --help
```

Refer to [conf/](conf/) for config & systemd service files. 
