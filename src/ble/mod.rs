//! Bluetooth LE for the game under Wine. `launch` replaces the game's BleWin10Lib DLLs
//! with shim/blewin10lib.c, which sends the game's requests to this bridge over an
//! AF_UNIX socket; the bridge does the Bluetooth work with BlueZ.

mod bridge;
mod mgmt;
mod protocol;

pub use bridge::serve;
