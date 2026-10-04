# Playing with friends (LAN and internet)

The game is server-authoritative over **UDP, port 7777** (change with `PORT=`). One person hosts; everyone else joins.

## Host

```bash
HOST=1 scripts/play.sh          # you + 7 bots, open to your network
HOST=1 BOTS=0 scripts/play.sh   # humans only
```

The script builds everything in `--release`, starts the server on `0.0.0.0:7777`, prints your LAN IP and the exact command
for friends, then opens your own game window. Closing the window (or Ctrl-C) stops the server.

Up to 16 players per server (bots count). Free-for-all by default; `scripts/play.sh` forwards nothing else, so for teams run
the server by hand: `target/release/shinobi-server 0.0.0.0:7777 --level levels/8v8.lvl --teams`.

## Join

```bash
scripts/join.sh 192.168.1.20            # the host's LAN IP
scripts/join.sh play.example.com 7777   # public address / forwarded port
LEVEL=levels/ffa_xl.lvl scripts/join.sh 192.168.1.20   # when the host runs the other map
```

The level must match the host's (`levels/ffa.lvl` is the default on both sides). Protocol versions must match too: both
sides must be on the same commit (a mismatch shows "update the client" on the kick screen).

## Same network (LAN)

Usually nothing else is needed. If friends cannot connect, the host's firewall is blocking UDP:

```bash
sudo ufw allow 7777/udp          # Ubuntu
sudo firewall-cmd --add-port=7777/udp --permanent && sudo firewall-cmd --reload   # Fedora
```

## Over the internet

1. Give the host machine a fixed LAN address (or a DHCP reservation in the router).
2. On the router, **forward UDP 7777** to the host's LAN IP.
3. Friends join your **public** IP (search "what is my ip") with `scripts/join.sh <public-ip> 7777`.
4. Behind carrier-grade NAT (some mobile / ISP lines) port forwarding is impossible: use a small VPS running the server, or
   a mesh VPN (Tailscale / ZeroTier) and join the VPN address as if it were a LAN.

Playing well needs under about 80 ms ping. The game shows your ping on the HUD; the server prints a telemetry line every
minute with each player's ping and prediction corrections.

## When something is wrong

Paste these three files from `playtest/` with your report:

* `server-<time>.log`: includes `strike: ...` lines (why the anti-cheat objected, if it ever does) and `kick player ...`.
* `game-<time>.log`: includes `PERF` lines (fps, frame-time spikes, how late the display clock runs).
* `telemetry-<time>.txt`: per-player shots, hit %, ping.

A kick screen can be left with Enter or Esc.
