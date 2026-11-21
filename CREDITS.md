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
| `core/src/drivers/hid_io.rs` | report conventions of `src/hid.h` / `src/hid.cpp` (`HidDev`) |
| `core/src/drivers/hyperx.rs` | `src/drivers/hyperx.cpp` |
| `core/src/drivers/steelseries_elite.rs` | `src/drivers/steelseries_elite.cpp` |
| `core/src/drivers/razer_headset.rs` | `src/drivers/razer_headset.cpp` |
| `core/src/drivers/barracuda_pro.rs` | `src/drivers/barracuda_pro.cpp` |
| `core/src/drivers/astro.rs` | `src/drivers/astro.cpp` |
| `core/src/drivers/audeze.rs` | `src/drivers/audeze.cpp` |
| `core/src/drivers/jabra.rs` | `src/drivers/jabra.cpp` |
| `core/src/drivers/jbl.rs` | `src/drivers/jbl.cpp` |
| `core/src/drivers/plantronics.rs` | `src/drivers/plantronics.cpp` |
| `core/src/drivers/turtlebeach.rs` | `src/drivers/turtlebeach.cpp` |
| `core/src/drivers/akg.rs` | `src/drivers/akg.cpp` |
| `core/src/drivers/nzxt.rs` | `src/drivers/nzxt.cpp` |
| `core/src/drivers/sony_inzone.rs` | `src/drivers/sony_inzone.cpp` |
| `core/src/drivers/fractal.rs` | `src/drivers/fractal.cpp` |
| `core/src/drivers/mchose_headset.rs` | `src/drivers/mchose_headset.cpp` |
| `core/src/drivers/logitech_g435_0adf.rs` | `src/drivers/logitech_g435_0adf.cpp`, `src/drivers/logitech_g435_0adf.h` |
| `core/src/drivers/endorfy.rs` | `src/drivers/endorfy.cpp` |
| `core/src/drivers/maono.rs` | `src/drivers/maono.cpp` |
| `core/src/drivers/alienware.rs` | `src/drivers/alienware.cpp` |
| `core/src/drivers/pressplay_icarus.rs` | `src/drivers/pressplay_icarus.cpp` |
| `app/BatteryLife.Logic/AlertRules.cs` | alert semantics of `src/alert_rules.h` |
| `core/src/drivers/report_io.rs` | HID conventions of `src/hid.h` (report lengths, read / write framing) |
| `core/src/drivers/razer.rs` | `src/drivers/razer.cpp` |
| `core/src/drivers/steelseries.rs` | `src/drivers/steelseries.cpp` |
| `core/src/drivers/corsair.rs` | `src/drivers/corsair.cpp` |
| `core/src/drivers/glorious.rs` | `src/drivers/glorious.cpp` |
| `core/src/drivers/pulsar.rs` | `src/drivers/pulsar.cpp` |
| `core/src/drivers/lamzu.rs` | `src/drivers/lamzu.cpp` |
| `core/src/drivers/vgn.rs` | `src/drivers/vgn.cpp` |
| `core/src/drivers/finalmouse.rs` | `src/drivers/finalmouse.cpp` |
| `core/src/drivers/wlmouse.rs` | `src/drivers/wlmouse.cpp` |
| `core/src/drivers/vaxee.rs` | `src/drivers/vaxee.cpp` |
| `core/src/drivers/ninjutso.rs` | `src/drivers/ninjutso.cpp` |
| `core/src/drivers/scyrox.rs` | `src/drivers/scyrox.cpp` |
| `core/src/drivers/endgame.rs` | `src/drivers/endgame.cpp` |
| `core/src/drivers/endgame_we.rs` | `src/drivers/endgame_we.cpp` |
| `core/src/drivers/zowie.rs` | `src/drivers/zowie.cpp` |
| `core/src/drivers/hidport.rs` | HID conventions of `src/hid.h` (`HidColl`, `HidDev`) |
| `core/src/drivers/keychron.rs` | `src/drivers/keychron.cpp` |
| `core/src/drivers/royal_kludge.rs` | `src/drivers/royal_kludge.cpp` |
| `core/src/drivers/nuphy.rs` | `src/drivers/nuphy.cpp` |
| `core/src/drivers/akko.rs` | `src/drivers/akko.cpp` |
| `core/src/drivers/aula.rs` | `src/drivers/aula.cpp` |
| `core/src/drivers/cherry.rs` | `src/drivers/cherry.cpp` |
| `core/src/drivers/atk_keyboard.rs` | `src/drivers/atk_keyboard.cpp` |
| `core/src/drivers/rongyuan.rs` | `src/drivers/rongyuan.cpp` |
| `core/src/drivers/sonix.rs` | `src/drivers/sonix.cpp` |
| `core/src/drivers/via_dongle.rs` | `src/drivers/via_dongle.cpp` |
| `core/src/drivers/semico.rs` | `src/drivers/semico.cpp` |
| `core/src/drivers/ajazz_ak992.rs` | `src/drivers/ajazz_ak992.cpp` |
| `core/src/drivers/mchose_keyboard.rs` | `src/drivers/mchose_keyboard.cpp` |
| `core/src/drivers/machenike.rs` | `src/drivers/machenike.cpp` |
| `core/src/drivers/niceboy.rs` | `src/drivers/niceboy.cpp` |
| `core/src/drivers/redragon_evision.rs` | `src/drivers/redragon_evision.cpp` |
| `core/src/drivers/dareu.rs` | `src/drivers/dareu.cpp` |
| `core/src/drivers/darkrim.rs` | `src/drivers/darkrim.cpp` |
| `core/src/drivers/hid_dev.rs` | HID conventions of `src/hid.h` / `src/hid.cpp` (HidColl, HidDev) |
| `core/src/drivers/rawm.rs` | `src/drivers/rawm.cpp`, `src/drivers/rawm_protocol.h` |
| `core/src/drivers/sprime.rs` | `src/drivers/sprime.cpp` |
| `core/src/drivers/darmoshark.rs` | `src/drivers/darmoshark.cpp` (PollDarmoshark, PollDarmosharkNrf) |
| `core/src/drivers/kysona.rs` | `src/drivers/kysona.cpp` |
| `core/src/drivers/chimera.rs` | `src/drivers/chimera.cpp` |
| `core/src/drivers/arbiter.rs` | `src/drivers/arbiter.cpp` |
| `core/src/drivers/atk_bitmouse.rs` | `src/drivers/atk_bitmouse.cpp` |
| `core/src/drivers/eweadn.rs` | `src/drivers/eweadn.cpp` |
| `core/src/drivers/zenith.rs` | `src/drivers/zenith.cpp` |
| `core/src/drivers/misc_mice.rs` | `src/drivers/misc_mice.cpp`, `src/drivers/xenta_protocol.cpp` / `.h`, `src/drivers/ardor_fury_v2_protocol.h` |
| `core/src/drivers/fantech.rs` | `src/drivers/fantech.cpp` |
| `core/src/drivers/raigor.rs` | `src/drivers/raigor.cpp` |
| `core/src/drivers/rapoo.rs` | `src/drivers/rapoo.cpp`, `src/drivers/rapoo_protocol.h` |
| `core/src/drivers/areson.rs` | `src/drivers/areson.cpp` |
| `core/src/drivers/coolermaster.rs` | `src/drivers/coolermaster.cpp` |
| `core/src/drivers/msi.rs` | `src/drivers/msi.cpp` |
| `core/src/drivers/rog.rs` | `src/drivers/rog.cpp` |
| `core/src/drivers/mchose.rs` | `src/drivers/mchose.cpp` |
| `core/src/drivers/mchose_a5.rs` | `src/drivers/mchose_a5.cpp` |
| `core/src/drivers/mchose_g3.rs` | `src/drivers/mchose_g3.cpp` |
| `core/src/drivers/mchose_modern.rs` | `src/drivers/mchose_modern.cpp` |
| `core/src/drivers/sinowealth_receiver.rs` | `src/drivers/sinowealth_receiver.cpp` |
| `core/src/drivers/aula_mouse.rs` | `src/drivers/aula_mouse.cpp`, `src/drivers/aula_mouse_protocol.h` |

BatteryHub in turn credits the Logitech HID++ 2.0 specification drafts, Solaar and HeadsetControl.
