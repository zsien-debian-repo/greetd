# greetd

greetd is a minimal and flexible login manager daemon that makes no assumptions about what you want to launch.

Use [gtkgreet](https://git.sr.ht/~kennylevinsen/gtkgreet) to launch [sway](https://github.com/swaywm/sway) if you want a fully graphical session, or use `agreety` to launch a shell if you want a drop-in replacement for `agetty(8)` and `login(1)`.

If you can run it from your shell in a TTY, greetd can start it. If it can be taught to speak a simple JSON-based IPC protocol, then it can be a greeter.

See the [wiki](https://man.sr.ht/~kennylevinsen/greetd) for FAQ, guides for common configurations, and troubleshooting information.

## List of known greetd greeters

- agreety - The simple, text-based greeter living in this repo is a simple example.
- [gtkgreet](https://git.sr.ht/~kennylevinsen/gtkgreet) - The flagship graphical, GTK based greeter (xdg-shell or wlr-layer-shell, to be used with something like `cage`)
- [dlm](https://git.sr.ht/~kennylevinsen/dlm) - Dumb Login Manager (using fbdev)
- [wlgreet](https://git.sr.ht/~kennylevinsen/wlgreet) - Wayland greeter (using wlr-layer-shell, to be used with something like `sway`)
- [tuigreet](https://github.com/apognu/tuigreet) - Console UI greeter (using tui-rs)

## Installation

The below will install greetd, agreety and the default configuration. This looks *just* like `agetty(8)` and `login(1)`. See the manpages and the wiki for information on how to do more interesting things.

### From packages

#### Arch Linux

greetd and a few greeters are available in AUR for Arch Linux.

#### Gentoo

```sh
emerge gui-libs/greetd
```

### Manually from source

```sh
# Compile greetd and agreety.
cargo build --release

# Put things into place
sudo cp target/release/{greetd,agreety} /usr/local/bin/
sudo cp greetd.service /etc/systemd/system/greetd.service
mkdir /etc/greetd
cp config.toml /etc/greetd/config.toml

# Create the greeter user
sudo useradd -M -G video greeter
sudo chown -R greeter:greeter /etc/greetd/

# Look in the configuration file `/etc/greetd/config.toml` and edit as appropriate.
# When done, enable and start greetd
systemctl enable --now greetd
```

## How do I write my own greeter?

All you need to do is an application that can speak the greetd IPC protocol, which is documented in `greetd-ipc(7)`. See gtkgreet or agreety for inspiration.
