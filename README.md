# moproxy

A transparent TCP to SOCKSv5 proxy on Linux written in Rust.

Basic features:

 * Transparent TCP proxy with `iptables -j REDIRECT`
 * Support multiple SOCKSv5 backend servers
 * SOCKS-layer alive & latency probe
 * Prioritize backend servers according to latency


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

