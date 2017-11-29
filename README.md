# moproxy

A transparent TCP to SOCKSv5/HTTP proxy on Linux written in Rust.

Features:

 * Transparent TCP proxy with `iptables -j REDIRECT`
 * Support multiple SOCKSv5/HTTP backend proxy servers
 * SOCKS/HTTP-layer alive & latency probe
 * Prioritize backend servers according to latency
 * Optional status web page


## Background

I need to proxy a list of websites on my home router.
[shadowsocks-libev](https://github.com/shadowsocks/shadowsocks-libev) is used
to setup encrypted SOCKSv5 tunnels between the router and destinations.
For availability, I ran multiple Shadowsocks servers on different places.
In previous solution, HAProxy ran on the router and distributed traffic came
from `ss-redir` (SS client that provide transparent proxy) to that servers.

However, issues existed: 1) HAProxy don't support TCP Fast Open on backend
side; 2) availability probe only done on TCP layer, not SOCKS layer; 3) I
actually don't need "load balance" but want always connect to one server
that not behind "traffic jam".

This project try to overcome above issues by catching TCP connections and
then redirect them to a group of `ss-local`, which connect to Shadowsocks
servers.

It's also a personal practice on Rust language. (The first workable program
I have written in Rust (

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
```

### Server list file
You may list all proxy servers in a text file to avoid a messy CLI arguments.

```ini
[server-1]
address=127.0.0.1:2001
protocol=socks5
[server-2]
address=127.0.0.1:2002
protocol=http
[backup]
address=127.0.0.1:2002
protocol=socks5
score base=5000 ;add 5k to pull away from preferred server.
```

Pass the file path to `moproxy` via `--list` argument.

## Details

### Latency probing

Just send a hard-coded DNS request to Google Public DNS (8.8.8.8) over SOCKS,
and take the delay of receiving a response.

### Priority of servers

Sort online servers by latency in
[exponential moving average](https://en.wikipedia.org/wiki/Moving_average#Exponential_moving_average).
If a server is down, put that in the end of list.
When a server just restore to online, add extra "latency" to the fisrt probe
result after down, so that servers with unstable network get penalty.

### SOCKS

Although I mentioned *SOCKSv5* repeatedly, it actually speak an variant that
Shadowsocks client use, not a real RFC 1928 protocol. It's mostly for lower
latency and simplify implementation, and may not work with other SOCKS server.

