# Migration guide


## v0.4 to v0.5

- Multiple values for single CLI argument now need to be delimited by comma

Before: `--port 2081 2082 2083`

After: `--port 2081,2082,2083`

- `listen ports =` on proxy list INI file is not longer supported.
  User should migrate them to proxy selection policy.

Before:
```ini
# Proxy list
[server-1]
listen ports=2081
# ...
[server-2]
listen ports=2082
# ...
```

After:
```ini
# Proxy list
[server-1]
capabilities = cap1
# ...
[server-2]
capabilities = cap2
# ...
```
```
# Ruleset
listen port 2081 require cap1
listen port 2082 require cap2
```

## v0.3 to v0.4

`-h` (listen host) has been renamed to `-b`

Before: 
```bash
moproxy -h ::1 -p 2080 ...
```

After:
```bash
moproxy -b ::1 -p 2080 ...
```
