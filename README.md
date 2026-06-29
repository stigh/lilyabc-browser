# lilyabc-browser

A lightweight desktop **browser/viewer for LilyPond (`.ly`) and ABC (`.abc`) sheet music**.

It does not engrave music itself — it shells out to the canonical engravers
(`lilypond`, `abcm2ps`) and displays their rendered output, with async rendering and a
content-hash cache. Built in Rust with [`egui`](https://github.com/emilk/egui).

## Status

Early development. See [`docs`/plan] for the milestone roadmap (M0 scaffold → M6 polish).

## Requirements

At **runtime** the app invokes these binaries (they are *not* bundled into the binary):

- [`lilypond`](https://lilypond.org/) — renders `.ly`
- [`abcm2ps`](https://github.com/lewdlume/abcm2ps) — renders `.abc`

On Debian/Ubuntu: `sudo apt install lilypond abcm2ps`.

## Build

### With Docker (no host toolchain needed)

```sh
# Compile-check only (fast):
docker build --target builder -t lilyabc-build .

# Full image (includes lilypond + abcm2ps):
docker build -t lilyabc-browser .
```

Running a GUI from the container needs the host display forwarded. On a Wayland session:

```sh
docker run --rm \
  -e WAYLAND_DISPLAY -e XDG_RUNTIME_DIR=/tmp \
  -v "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY:/tmp/$WAYLAND_DISPLAY" \
  -v "$PWD/samples:/samples:ro" \
  lilyabc-browser
```

(or via XWayland: `-e DISPLAY -v /tmp/.X11-unix:/tmp/.X11-unix` after `xhost +local:`).

### Native (faster dev loop)

Needs a modern Rust toolchain (`rustup`, MSRV 1.92) and the egui build deps:

```sh
sudo apt install pkg-config libfontconfig1-dev libxcb-render0-dev \
    libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev libwayland-dev
cargo run --release
```

## License

MIT OR Apache-2.0.
