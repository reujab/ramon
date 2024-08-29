# Radon

> :warning: This project is a WIP and not yet functional. All configuration is subject to change.

Radon is a lightweight server monitoring framework. It sends notifications (e.g. emails) when certain conditions are met. For example, you can configure Radon to send an email every time an SSH connection from a new IP address is established. Or send an email when a service goes down.

Radon's design is heavily inspired by [fail2ban] and borrows features from other monitoring frameworks such as [Prometheus], though is certainly not a replacement.

## Examples

### Setup

```toml
[notify.default]
smtp = "localhost:587"
from = "radon@${host}"
to = "you@${host}"
# Send no more than 4 emails per minute.
limit = "4/m"
# When an notification is dispatched, wait 10 seconds to see if
# another action of the same type occurs, and aggregate
# them into one notification. If this process repeats for
# longer than 1 minute, send the notification anyway.
aggregate = "10s"
aggregate_timeout = "1m"
notify = "default"

# Aggregate all info notifications, and send them all in
# one at 8:00AM daily.
[notify.type.info]
aggregate = "* * * 8:00AM"

# Do not aggregate critical actions.
[notify.type.critical]
aggregate = "0s"
```

### SSH

```toml
# Log each login from a new IP
[var]
ssh_ips = { length = 64, store = true }

[monitor.ssh_login]
service = "ssh"
match_log = """^.*\]: Accepted \S+ for (?<user>\S+) from (?<ip>)"""
if = { "!ssh_ips" = "${ip}" }
push = { ssh_ips = "${ip}" }
notify = { type = "critical", title = "New SSH login from ${ip} to ${user}@${host}" }

# Alternatively, log every login
[monitor.ssh_login]
service = "ssh"
match_log = """^.*\]: Accepted \S+ for (?<user>\S+) from (?<ip>)"""
[[monitor.ssh_login.actions]]
# Check if the `ssh_ips` list contains the IP.
if = { ssh_ips = "${ip}" }
# We only send these info emails once per day (see Setup)
notify = { type = "info", title = "SSH login to ${user}@${host}" }
[[monitor.ssh_login.actions]]
if = { "!ssh_ips" = "${ip}" }
push = { ssh_ips = "${ip}" }
notify = { type = "critical", title = "New SSH login from ${ip} to ${user}@${host}" }
```

### System resources

```toml
[monitor.cpu]
cpu = ">90"
duration = "2m"
notify = { type = "warn", title = "[${host}] CPU > 90% for 2m" }
cooldown = "1h"

[monitor.ram]
ram = ">90"
swap = ">50"
notify = { type = "warn", title = "[${host}] RAM: ${ram}, swap: ${swap}" }
cooldown = "1h"
```

### Nginx

```toml
[var]
nginx_log = "/var/log/nginx/access.log"

[monitor.nginx_5xx]
log = "${nginx_log}"
match_log = """^\S+ \S+ \S+ \[.+\] ".*" (?<code>5\d{2})"""
notify = { type = "error", title = "Server error ${code} returned from nginx." }

# Report 404s only if it's a human
[var]
humans = { length = 64 }

[monitor.mark_human]
log = "${nginx_log}"
match_log = """^(?<ip>\S+) \S+ \S+ \[.+\] ".*\.css" 200"""
push = { humans = ip }

[monitor.nginx_404]
log = "${nginx_log}"
match_log = """^(?<ip>\S+) \S+ \S+ \[.+\] "(?<path>.*)" 404"""
if = { humans = ip }
action = { name = info, title = "404 at ${path}" }
```

### HTTP

```toml
[monitor.example_endpoint]
every = "5m"
get = "https://example.com/endpoint"
if = { "!err" = "" }
notify = { type = "error", title = "${url}: ${err}" }
```

### systemd

```toml
[monitor.services]
on = [ "service_fail" ]
notify = { type = "error", title = "Service failed: ${service}" }

[monitor.critical_service]
service = "criticald"
on = [ "service_fail" ]
notify = { type = "critical", title = "Critical service failed! Restarting..." }
```

### System integrity

```toml
[monitor.etc_passwd]
watch = "/etc/passwd"
notify = { name = "critical", title = "File changed: /etc/passwd" }

[monitor.ports]
on = [ "port_open" ]
notify = { name = "critical", title = "New port opened: $port" }
```
