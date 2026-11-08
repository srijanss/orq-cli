# Reading the review inbox from a phone

`orq remote serve` starts a small read-only web page that lists the review inbox, so you can check a run's report from a phone. Nothing on it can change a run: it only shows what Orq already stored. Merging happens at the provider, after following the link in a report.

## 1. Turn remote control on

Set `remote_control` in `.orq/run.toml` (checked in) or `<git-dir>/orq/run.toml` (private):

```toml
remote_control = true
```

or set `ORQ_REMOTE_CONTROL=1` in the environment. With it off, the command refuses to start.

## 2. Create the token file

The page is protected by one shared token, kept in `remote-token` in Orq's config directory (`$XDG_CONFIG_HOME/orq/`, else `~/.config/orq/`). It must be a regular file you own, at least 32 characters with no spaces, and readable by you alone:

```sh
mkdir -p ~/.config/orq
head -c 32 /dev/urandom | base64 | tr -d '/+=\n' > ~/.config/orq/remote-token
chmod 600 ~/.config/orq/remote-token
```

The command never prints the token. Copy it to your phone from the file.

## 3. Start the server

```sh
orq remote serve --repo .
```

It prints `listening on http://127.0.0.1:7878` and runs until Ctrl-C. It refuses to start, with one line and a non-zero exit, when remote control is off, the token file is missing, too short or too open, or the address cannot be bound.

To listen on another address, such as your Tailscale address, name it:

```sh
orq remote serve --repo . --listen 100.64.0.1:7878
```

## 4. Reach it over Tailscale

Install Tailscale on the machine and the phone, then open `http://<tailscale-address>:7878` in the phone's browser. The server answers only loopback and Tailscale addresses (100.64.0.0/10 and fd7a:115c:a1e0::/48); every other address is refused. It is plain HTTP, so keep it on the tailnet and never expose it to the internet.

## 5. What the page shows

Enter the token (it is held in memory only, so reloading asks again). The page lists each run with its feature, status and risk. Tap a run for its summary, risks, files, tests and review rounds. A report that no longer matches the provider's head is marked "stale, may be out of date" and carries no link. The page is read-only: it makes only GET requests and has no button that approves, merges or changes anything.

## 6. How to rotate the token

Write a new token to the same file with mode 600, then stop the server with Ctrl-C and start it again. The old token stops working at once, and the page asks for the new one.
