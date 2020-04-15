# moproxy

A transparent TCP to SOCKSv5/HTTP proxy on *Linux* written in Rust.

Features:

 * Transparent TCP proxy with `iptables -j REDIRECT`
 * Support multiple SOCKSv5/HTTP backend proxy servers
 * SOCKS/HTTP-layer alive & latency probe
 * Prioritize backend servers according to latency
 * Full IPv6 support
 * Multiple listen ports, each for a subset of proxy servers
 * Remote DNS resolving for TLS with SNI (extract domain name from TLS
   handshaking)
 * Optional try-in-parallel for TLS (try multiple proxies and choose the one
   first response)
 * Optional status web page (latency, traffic, etc. w/ curl-friendly output)
 * Optional [Graphite](https://graphite.readthedocs.io/) and
   [Prometheus](https://prometheus.io/) support
   (to build fancy dashboard with [Grafana](https://grafana.com/) for example)
 * Customizable proxy selection algorithm with Lua script (see
   `conf/simple_scroe.lua`).

```
+------+  TCP  +----------+       SOCKSv5   +---------+
| Apps +------>+ iptables |    +------------> Proxy 1 |
+------+       +----+-----+    |            +---------+
           redirect |          |
                 to v          |      HTTP  +---------+
               +----+----+     |   +--------> Proxy 2 |
               |         +-----+   |        +---------+
               | moproxy |---------+             :
               |         +------------...        :
               +---------+  choose one  |   +---------+
                I'M HERE                +---> Proxy N |
                                            +---------+
```

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
iptables -t nat -A OUTPUT -p tcp -m multiport --dports 80,443 -j REDIRECT --to-port 2080
# redirect connections initiated by other hosts (if you are router)
iptables -t nat -A PREROUTING -p tcp -m multiport --dports 80,443 -j REDIRECT --to-port 2080

# or the nft equivalent
nft add rule nat output tcp dport {80, 443} redirect to 2080
nft add rule nat prerouting tcp dport {80, 443} redirect to 2080
```

### Server list file
You may list all proxy servers in a text file to avoid messy CLI arguments.

```ini
[server-1]
address=127.0.0.1:2001 ;required
protocol=socks5 ;required

[server-2]
address=127.0.0.1:2002
protocol=http
test dns=127.0.0.53:53 ;use remote's local dns server to caculate delay
listen ports=8001

[server-3]
address=127.0.0.1:2003
protocol=http
; server-3 serves for port 8001 & 8002, while server-2 is only for
; port 8001. server-1 accepts connections coming from any ports specified
; by CLI argument --port.
listen ports=8001,8002

[backup]
address=127.0.0.1:2002
protocol=socks5
score base=5000 ;add 5k to pull away from preferred server.
```

Pass the file path to `moproxy` via `--list` argument.

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
address-based selection on the firewall.

### Monitoring
Metrics (latency, traffic, number of connections, etc.) are useful for
diagnosis and customing your own proxy selection. You can access these
metrics with variety of methods, from a simple web page, curl, to specialized
tools like Graphite or Prometheus.

`--stats-bind [::1]:8080` turn on the internal stats page, via HTTP, on the
given IP address and port number. It returns a HTML page for web browser,
or a ASCII table for `curl`.

The stats page only provide current metrics and a few aggregation. Graphite
(via `--graphite`) or Prometheus (via `--stats-bind` then `\metrics`) should
be use if you want the full history.

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

Refer to `conf/` for config & systemd service files. 
