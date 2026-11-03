# Start voytty at login

Automatic startup is off by default. Installation and `voytty setup` do not
enable it. After setup, run this on each computer where you want it:

```sh
voytty daemon autostart enable
```

This takes effect at the next login. It does not start the daemon immediately
or open a terminal session. To start a configured, stopped daemon now, run
`voytty daemon restart`.

Check or disable automatic startup:

```sh
voytty daemon autostart status
voytty daemon autostart disable
```

Disabling startup does not stop a running daemon or its sessions. Use
`voytty daemon stop` to stop it; this ends its sessions and asks for confirmation
when any are running.

- **macOS:** starts at user login through launchd.
- **Linux:** requires a systemd user manager and the standard configuration
  location; a custom `XDG_CONFIG_HOME` is not supported. Startup does not change
  the system's linger policy.
- **Windows:** starts at user login through the current user's Run registry entry.

Updates keep the registration. If you move the executable, run `enable` again.
`voytty reset` and `voytty uninstall` remove the registration.
