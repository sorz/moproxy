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

This project try to overcome above issues. And also a personal practice on
Rust language. (It's the first workable program I have written in Rust (

