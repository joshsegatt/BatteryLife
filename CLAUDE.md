# BatteryLife

Battery hub for wireless devices on Windows 11. GPL-3.0. Read-only: never write settings to a device.

## Layout
- `core/`: Rust engine. Drivers per vendor, declared in `core/src/registry.rs` (an exact product id beats a whole-vendor match; `CHAINS` lists the drivers tried in turn on a vendor's other products). Run as a child process: `batterylife-core --watch` prints one JSON array per line.
- `app/`: C# / WinUI 3 tray app (`BatteryLife.App`), with its UI rules in `BatteryLife.Logic` (tested in `BatteryLife.Logic.Tests`).
- Design: `PRODUCT.md`, `DESIGN.md`, direction contract in `.impeccable/surfaces/flyout.md` (airport-sign world: yellow only when something needs charging).

## Checks (CI runs the same)
- `cd core && cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked`
- `cd app && dotnet test BatteryLife.Logic.Tests && dotnet build BatteryLife.App -c Release -r win-x64 -p:Platform=x64 -warnaserror`
- Run the app against real devices: `cargo build --release` in `core/`, then build the app (it copies `batterylife-core.exe`). `BatteryLife.exe --demo [--calm] --open` shows synthetic data.
- Release: pushing a tag `vX.Y.Z` runs `.github/workflows/release.yml`, which publishes a portable zip with .NET inside. Use `dotnet build -p:SelfContained=true`, not `dotnet publish`: publish drops the `.xbf` and `.pri` files and the exe crashes on start.

## Conventions
- Drivers ported from BatteryHub keep a header naming the source file and commit, and are listed in `CREDITS.md`.
- Commits and PRs carry only the author (Josh); no AI co-author or "generated with" lines.
- One branch and PR per task; squash-merge after green CI and a code review.
