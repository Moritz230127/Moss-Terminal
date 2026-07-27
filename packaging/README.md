# packaging/

Optional, manually-installed integration points for keeping a Moss Terminal
checkout in sync with the system `kitty` package on Arch. **Nothing in this
directory is installed automatically** by `scripts/moss-setup.py` or by
anything else in this repo — you decide whether to install any of it, and
you install it yourself with the commands below.

## Files

- **`moss-kitty-sync.hook`** — a pacman hook. When pacman upgrades the
  `kitty` package, it touches `/var/tmp/moss-kitty-update-pending` as root
  and does nothing else (see the comments in the file for why it's kept
  that minimal — running user/build code as root is unsafe). Picking that
  flag up and actually resyncing is left to you, either by hand or via the
  timer below.

- **`moss-sync-check.service`** + **`moss-sync-check.timer`** — a systemd
  **user** service/timer pair. The timer fires daily and runs
  `scripts/moss-sync-kitty.sh --notify-only`, which compares the kitty
  version this tree was built against with the version Arch currently
  ships (or the latest GitHub release if pacman isn't available) and prints
  a message / sends a desktop notification if they differ. It never
  downloads, patches, builds, or swaps anything in on its own.

To actually pull in a new kitty version, run (from the repo root, as your
normal user, not via the timer):

```
scripts/moss-sync-kitty.sh --version X.Y.Z          # rehearse: fetch, patch, build, test out-of-tree
scripts/moss-sync-kitty.sh --version X.Y.Z --adopt   # after a green rehearsal, swap it into kitty/
```

See `scripts/moss-sync-kitty.sh --help` and `scripts/moss-apply-patches.sh`
for the full set of flags.

## Installing the pacman hook (optional)

```
sudo install -Dm644 packaging/moss-kitty-sync.hook /etc/pacman.d/hooks/moss-kitty-sync.hook
```

Remove it with `sudo rm /etc/pacman.d/hooks/moss-kitty-sync.hook`.

## Installing the systemd user timer (optional)

```
mkdir -p ~/.config/systemd/user
cp packaging/moss-sync-check.service packaging/moss-sync-check.timer ~/.config/systemd/user/
systemctl --user enable --now moss-sync-check.timer
```

`moss-sync-check.service` hard-codes the repo path via `MOSS_REPO=%h/Desktop/智能体开发/moss-terminal-kitty`
in its `Environment=` line — edit that if your checkout lives somewhere
else before enabling the timer.

Remove with:

```
systemctl --user disable --now moss-sync-check.timer
rm ~/.config/systemd/user/moss-sync-check.service ~/.config/systemd/user/moss-sync-check.timer
```

## Warning

Everything in this directory is optional and inert until you run the
install commands above yourself. `scripts/moss-setup.py` does not touch
`/etc/pacman.d/hooks/` or `~/.config/systemd/user/` and never runs anything
as root.
