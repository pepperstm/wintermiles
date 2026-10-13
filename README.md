# WinterMiles

Ride any GPX route on your indoor trainer. WinterMiles turns a route file into a 3D world, drives your smart trainer's resistance from the real gradient, and makes a game of it with ghosts, pace riders, timed climbs, XP and badges.

Open source (MIT) for **Windows, macOS and Linux**. Written in **Rust**, with a thin web layer for drawing. Not affiliated with icTrainer or any other commercial product.

## What it does

- **GPX in, virtual ride out.** Import routes from Strava, Komoot, RideWithGPS or a bike computer. Routes with no elevation data (planner exports) get it filled in automatically.
- **A 3D version of the real place.** The road follows your GPX; the surrounding landscape is built from open elevation data, so the mountains on the horizon are the real ones, and the towns and villages you pass through are built from OpenStreetMap's building outlines. A live OpenStreetMap minimap and a gradient-coloured elevation profile sit alongside. Works offline with generated hills.
- **Smart trainer control.** Gradient simulation (with a "trainer difficulty" setting) or ERG mode, over Bluetooth FTMS, Tacx FE-C-over-Bluetooth, or ANT+ FE-C.
- **Sensors.** Power meters, heart-rate straps, speed and cadence sensors over Bluetooth and ANT+. When several devices report the same metric the most direct one wins (crank power meter over trainer estimate, strap over relayed HR). Basic trainers with only a speed sensor can use estimated power.
- **Gamification.** Race the ghost of your best time on each route, chase pace riders at fixed W/kg, get timed on every auto-detected climb (categorised 4 → HC), and earn XP, levels and badges.
- **Recording and sharing.** Every ride is saved as a standard `.FIT` file (GPS, power, heart rate, cadence, laps, normalised power) and can be uploaded to Strava as a Virtual Ride in one click or automatically.
- **Apple Health.** Import your Health export to set weight, FTP and max heart rate; use an Apple Watch as a live heart-rate sensor; send rides to Health via the FIT file. See [the limits](#apple-health-and-apple-watch) below.
- **Demo rider.** A simulated rider and trainer so you can try everything with no hardware.

## How it is built

About two thirds of the code is Rust, and everything that computes or talks to hardware is in it:

```
crates/core/      Pure Rust library, no I/O — fully unit-tested
  gpx, physics    GPX → resampled route, smoothing, gradients, climb detection; speed-from-power model
  ride, gamify    The ride simulation and recorder; XP, levels, badges
  fit             FIT activity encoder
  devices/        Bluetooth GATT and ANT+ protocol encode/decode; the sensor hub
  world/          Terrain: elevation tiles, height field, chunk / horizon / road meshes;
                  OpenStreetMap buildings (Overpass JSON and vector-tile decoding, extrusion)
  health          Streaming scanner for Apple Health exports
src-tauri/        The app (Rust, Tauri 2)
  devices/        Bluetooth via the OS stack (btleplug); ANT+ USB stick via libusb; demo rider
  session         50 Hz simulation loop, trainer control, saving rides
  terrain, buildings, strava, health, store, secrets
web/              The window contents (TypeScript, React, three.js)
  world/          Uploads the meshes Rust produced to the GPU; camera; rider models
  ui/             Screens and the ride HUD
```

The web layer contains no ride logic: it receives state from Rust about 20 times a second, glides between updates, and draws. Terrain meshes are generated on Rust worker threads, so building landscape never stalls a frame. The window is the operating system's own webview (WebKit on macOS/Linux, WebView2 on Windows) rather than a bundled browser.

Measured on an Apple Silicon Mac, against the earlier Electron/TypeScript prototype of the same app:

| | Electron prototype | Rust + system webview |
| --- | --- | --- |
| App bundle | 290 MB | **11 MB** |
| Memory during a ride with real terrain | not measured | ~95 MB app + ~140 MB system webview processes |

A fully native renderer (wgpu or Bevy) could replace the web layer later; because the core crate has no dependency on the shell, that is a contained change.

## Project status

Be aware of what has and has not been proven:

| Area | Status |
| --- | --- |
| GPX parsing, route building, climb detection, physics, ride recording, gamification, terrain and building generation, Health export scanning | Unit-tested (`cargo test`, 41 tests) |
| FIT export | Unit-tested by decoding the output with an independent FIT parser |
| Bluetooth and ANT+ **protocol** encoding/decoding (FTMS, Cycling Power, CSC, HR, ANT framing, ANT+ HR/power/speed/cadence/FE-C pages) | Unit-tested against hand-built packets |
| Full app: import → real-terrain download → ride → save FIT → personal best → summary | Verified by automated runs with the demo rider on macOS (Apple Silicon), in debug, dev-server and release builds |
| OpenStreetMap buildings | Verified against the live services on one Alpine route, through both data sources. Dense cities and very long routes have not been tried. |
| Bluetooth and ANT+ with **real hardware** | **Not yet tested.** Written to the published specifications, but no trainer, sensor or ANT+ stick has been connected to it — not even a scan has been run. Expect to find bugs; please report them with your device model. |
| Strava connect and upload | **Not yet tested against the live API** (needs a personal API key). |
| Apple Health import | Scanner unit-tested; not yet run on a real multi-GB export. |
| Windows and Linux | **Not yet built or run.** All dependencies are cross-platform, but only macOS has been exercised. |

## Getting started

You need [Rust](https://rustup.rs) (stable) and [Node.js](https://nodejs.org) 20+.

- **Linux** also needs the WebKitGTK and device libraries: `sudo apt install libwebkit2gtk-4.1-dev build-essential libssl-dev libayatana-appindicator3-dev librsvg2-dev libdbus-1-dev libudev-dev pkg-config`
- **Windows** needs the MSVC build tools; WebView2 ships with Windows 10/11.

```bash
npm install
npm run dev        # run with hot reload
npm test           # Rust unit tests
npm run build      # optimised app + installer for this platform (target/release/bundle/)
```

Then: **Routes → Import GPX…**, **Devices → connect your trainer**, and press **Ride**. No hardware handy? Press Ride on the built-in Demo Loop and choose **Use demo rider** (↑/↓ change its effort).

Ride keys: `Space` pause · `C` camera · `↑`/`↓` ERG target (or demo rider effort).

To inspect a recorded ride: `cargo run -p wintermiles-core --example fitinfo -- path/to/ride.fit`.

## Connecting hardware

### Bluetooth

**Devices → Add Bluetooth device…** lists nearby trainers and sensors. Supported: FTMS smart trainers (Wahoo, Elite, Saris, Zwift, JetBlack and most others made since ~2018), Tacx trainers using FE-C over Bluetooth, Cycling Power meters, heart-rate monitors, and speed/cadence sensors.

- A Bluetooth sensor can usually talk to only one app at a time: close Zwift, the manufacturer's app, etc.
- **macOS:** the first scan asks for Bluetooth permission.
- **Linux:** needs BlueZ running.
- Not supported: older Wahoo KICKRs that only speak Wahoo's proprietary protocol (use ANT+ FE-C for those).

### ANT+

Plug in an ANT+ USB stick (Garmin/Dynastream ANTUSB2 or ANTUSB-m) and press **Devices → Start ANT+ search**. WinterMiles listens for one device of each kind — trainer (FE-C), power meter, heart rate, speed, cadence, speed+cadence — and adds each as it is heard.

The stick is driven directly through libusb, with no vendor SDK:

- **Windows:** the stick must use the generic **WinUSB** driver. Install it once with [Zadig](https://zadig.akeo.ie) (select the ANT stick → WinUSB → Replace Driver). Note this stops Garmin Express using the stick until you switch the driver back.
- **Linux:** allow non-root access: `sudo cp resources/99-ant-usb.rules /etc/udev/rules.d/ && sudo udevadm control --reload`, then replug. (The `.deb` package installs this rule for you.)
- **macOS:** no setup, but quit Garmin Express — only one program can hold the stick.

Current limits: one ANT+ device per type (the first one heard), and no pairing to a specific device number yet.

## Strava

Strava issues API keys per application, and a secret can't be shipped inside an open-source app, so each user creates a free personal key once:

1. Go to <https://www.strava.com/settings/api> and create an application (any name/website).
2. Set **Authorization Callback Domain** to `localhost`.
3. In WinterMiles → **Settings → Strava**, paste the Client ID and Client Secret and press **Connect**.

Your browser opens for approval. The key and tokens are stored in the system keychain on macOS and Windows; on Linux they are kept in a file in the app's data folder readable only by your user. Rides upload as Virtual Rides with the route map.

## Apple Health and Apple Watch

Apple only allows apps running **on the iPhone** to read or write Health data — there is no Health access for desktop apps on any platform, including macOS. So WinterMiles integrates in the ways that are actually possible from a computer:

- **Read your data:** iPhone → Health → profile picture → *Export All Health Data*. Import the resulting `export.zip` under **Settings → Apple Health** to pull in your latest weight, cycling FTP, resting heart rate, VO₂max and the highest heart rate recorded in the last year. The file is read locally and nothing is uploaded.
- **Live Apple Watch heart rate:** the watch can't broadcast heart rate natively. Run a broadcaster app on it (e.g. HeartCast or BlueHeart) and it appears as a normal Bluetooth heart-rate sensor.
- **Write rides to Health:** set **Settings → Ride files** to an iCloud Drive folder, then open the `.FIT` file on the iPhone with an importer such as HealthFit or RunGap, which writes the workout into Health.

Direct two-way sync would need a companion iPhone app using HealthKit. That is the top item on the roadmap.

## Notes on the model

- **Speed is simulated from power**, not read from the trainer: `P = (m·g·(sinθ + Crr·cosθ) + ½·ρ·CdA·v²)·v`, with air density falling with altitude. Trainer difficulty changes only what the trainer feels like, never your virtual speed.
- **Terrain** near the road is pinned to the route's own elevation so the road never floats or tunnels, and blends into real elevation data further out. Where hairpin arms sit almost on top of each other, the ground takes the lower arm's level and the upper arm stands on a retaining wall.
- **Buildings** come from OpenStreetMap. The first time you ride a route, the outlines within about 350 m of the road are downloaded and kept on your computer (refreshed twice a year). The detailed source (the Overpass API) includes mapped heights and storey counts but runs on overloaded volunteer servers, so if none answers within a few seconds the app uses OpenStreetMap's vector tiles instead — outlines only, with heights estimated from footprint size — and tries the detailed source again a fortnight later. Simple rectangular houses get pitched roofs; everything else is a flat-roofed block. Anything mapped on top of the road itself is left out. Fetching buildings sends the route's coordinates to those OpenStreetMap servers; turn it off under **Settings → Virtual world** if you prefer.
- **Elevation is smoothed** (~60 m) before gradients are computed, so noisy GPS altitude doesn't make the trainer twitch. Routes with their own elevation data give better gradients than ones filled in from terrain data, which is coarse on steep hillsides.

## Roadmap

- Hardware testing across trainers and sensors; pairing to specific ANT+ devices; multiple devices per type
- Companion iPhone/Apple Watch app for real HealthKit sync and native watch heart rate
- Native renderer in place of the webview
- Structured workouts (ERG intervals) along a route
- More scenery from OpenStreetMap (forests, water, bridges and tunnels); mapped roof shapes
- Riding with friends over the network
- Wahoo legacy protocol, steering and virtual gears

## Data and credits

- Map tiles and building data © [OpenStreetMap](https://www.openstreetmap.org/copyright) contributors (ODbL), via the OSM tile servers, the public [Overpass API](https://wiki.openstreetmap.org/wiki/Overpass_API) instances and [vector.openstreetmap.org](https://vector.openstreetmap.org). Heavy users should configure their own servers, per the OSM usage policies.
- Terrain from the [Terrain Tiles](https://registry.opendata.aws/terrain-tiles/) open dataset (Tilezen/Mapzen; sources include SRTM, USGS 3DEP, EU-DEM and others).
- ANT+ access uses the ANT+ managed network key, which is issued to ANT+ adopters. If you distribute builds, register as an adopter (free) at [thisisant.com](https://www.thisisant.com).

## License

MIT — see [LICENSE](LICENSE).
