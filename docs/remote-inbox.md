# Orq's web: `orq serve`

`orq serve` starts Orq's web for this machine: every project, its features, its runs with their logs, the Reviews page, settings and storage. Open it on the machine or, over Tailscale, from a phone.

By default the web only shows. With **Remote actions** turned on (Settings, confirmed with your passkey), it can also act: Start, Pause, Resume and the other run actions, resolving, dismissing and reopening reviews, and cleaning up storage. While it runs, `orq serve` also looks at open reviews at their provider again every 30 seconds, so the Reviews page stays current.

You sign in with a token on a plain connection, or with a passkey over HTTPS (section 7).

## 1. Start it

From inside a repository:

```sh
orq serve --repo .
```

`--repo` defaults to the current directory. The server listens on `127.0.0.1:7878` unless told otherwise with `--listen`. It prints `listening on http://127.0.0.1:7878`, where the token is stored and the command that reads it, then runs until Ctrl-C. If there is no token yet, it creates one, private to you. It refuses to start, with one line and a non-zero exit, when the token file is too short, too open or a link, or the address cannot be bound.

`remote_control` (in `.orq/run.toml`, or `ORQ_REMOTE_CONTROL=1`) is Claude Remote Control for a run's sessions. `orq serve` does not read it: running the command is the opt-in.

## 2. Choose your own token

The token is kept in `remote-token` in Orq's config directory (`$XDG_CONFIG_HOME/orq/`, else `~/.config/orq/`). `orq serve` makes one for you, but you can also write your own. It must be a regular file you own, at least 32 characters with no spaces, and readable by you alone. Create it private from the first byte, in the directory the server reads. The new token goes into a fresh file (mode 600, made by the command below) that is then renamed over `remote-token`, so a token file that already exists, with loose permissions or as a symlink, is replaced and never written through. A symlink is removed first (`mv` would otherwise move the file into a linked directory), each generation step is run on its own so a failure in any of them stops the commands, a directory at that path is refused, and the old token is kept untouched if generating a new one fails:

```sh
(
  set -eu
  umask 077
  config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/orq"
  mkdir -p "$config_dir"
  tmp="$(mktemp "$config_dir/remote-token.XXXXXX")"
  trap 'rm -f "$tmp"' EXIT
  head -c 32 /dev/urandom > "$tmp"
  encoded="$(base64 < "$tmp")"
  token="$(printf '%s' "$encoded" | tr -d '/+=\n')"
  [ "${#token}" -ge 32 ] || { echo "could not generate a token" >&2; exit 1; }
  printf '%s' "$token" > "$tmp"
  chmod 600 "$tmp"
  [ ! -L "$config_dir/remote-token" ] || rm -f "$config_dir/remote-token"
  [ ! -d "$config_dir/remote-token" ] || { echo "$config_dir/remote-token is a directory" >&2; exit 1; }
  mv -f "$tmp" "$config_dir/remote-token"
)
```

The command never prints the token. Copy it to your phone from the file.

## 3. Start the server on your Tailscale address

A server listening on `127.0.0.1` (the default) cannot be reached from a phone, so name the machine's Tailscale address. Find it with `tailscale ip -4`, then:

```sh
orq serve --repo . --listen 100.64.0.1:7878
```

Use your own address in place of `100.64.0.1`. It prints `listening on http://100.64.0.1:7878` and runs until Ctrl-C.

## 4. Reach it over Tailscale

Install Tailscale on the machine and the phone, then open the printed address in the phone's browser. The server answers only loopback and Tailscale addresses (100.64.0.0/10 and fd7a:115c:a1e0::/48); every other address is refused. It is plain HTTP, so keep it on the tailnet and never expose it to the internet. For daily use, prefer HTTPS with a passkey (section 7).

## 5. Signing in with the token

Enter the token (it is held in memory only, so reloading asks again). The web lists your projects; open one for its features, runs and logs, or open the Reviews page for each run's summary, risks, files, tests and review rounds. A report that no longer matches the provider's head is marked "stale, may be out of date" and carries no link. Turning remote actions on needs a passkey sign-in.

## 6. How to rotate the token

Write a new token to the same file with mode 600 (section 2), then stop the server with Ctrl-C and start it again. The old token stops working at once, and the page asks for the new one.

## 7. Sign in with a passkey over HTTPS

The token above travels in plain HTTP. For daily use, put the page behind HTTPS and sign in with a passkey instead. Browsers only offer passkeys on a host name over HTTPS, which is what `tailscale serve` gives you: it publishes the local server at `https://<machine>.<tailnet>.ts.net` and terminates TLS for you. The machine name and tailnet are in `tailscale status`.

Run the server on the loopback address and give it that host name with `--passkey-host` (only the bare name, with no scheme, port or path):

```sh
orq serve --repo . --listen 127.0.0.1:7878 --passkey-host box.tail1234.ts.net
```

`orq serve` runs until Ctrl-C, so leave it running and, in a second terminal, publish it over HTTPS:

```sh
tailscale serve --bg 7878
```

Use your own host name in place of `box.tail1234.ts.net`. Open `https://box.tail1234.ts.net` on the phone.

### The first passkey

Until a passkey exists, the page asks for the token once. After you are signed in with it, the page offers to add a passkey for this device. This is the only thing the token can do through the proxy: the first passkey. From then on the page shows only the passkey button, and the token no longer opens the page or the API over `tailscale serve`.

### More passkeys

Once signed in with a passkey, add another device's passkey from the page while you are signed in. The token cannot add one. List what is registered with:

```sh
orq remote passkey list
```

### Direct connections on this machine

By default the token still works for a direct connection from the machine itself (not through `tailscale serve`). To turn that off too, so a passkey session is the only way in:

```sh
orq serve --repo . --passkey-host box.tail1234.ts.net --no-loopback-token
```

### Recovery: a lost device

Passkeys are removed on the machine, by label, never from the page:

```sh
orq remote passkey remove phone
```

Sessions signed in with that passkey end at once. If you remove the last passkey, nobody is signed in and the token can enrol a new first passkey again, so removing them all is the way back in when every device is lost.
