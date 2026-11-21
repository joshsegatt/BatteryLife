# Credits

BatteryLife is licensed under the GNU General Public License v3.0 (see `LICENSE`).

Parts of the device protocol logic are ported from **BatteryHub** (formerly Wireless Battery) by krisgfx9 and
contributors, https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0. They were rewritten in Rust
or C#, and each ported file names its source in its header comment.

| BatteryLife file | Ported from (BatteryHub, commit `fd2267d`) |
|---|---|
| `core/src/drivers/logitech.rs` | `src/drivers/logitech.cpp` |
| `core/src/drivers/phonelink.rs` | `src/drivers/phonelink.cpp`, `src/drivers/phonelink_protocol.h` |
| `core/src/drivers/bluetooth.rs` | `src/drivers/generic.cpp` (PollBluetooth) |
| `core/src/drivers/generic_hid.rs` | `src/drivers/generic.cpp` (PollGenericHid) |
| `core/src/drivers/xbox.rs` | `src/drivers/xinput.cpp` |
| `app/BatteryLife.Logic/AlertRules.cs` | alert semantics of `src/alert_rules.h` |

BatteryHub in turn credits the Logitech HID++ 2.0 specification drafts, Solaar and HeadsetControl.
