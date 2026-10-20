//! MetaRecover core — deleted file recovery engine.
//!
//! * `device`  – sector-aligned raw device / image access
//! * `volume`  – filesystem detection, MBR/GPT partitions
//! * `ntfs`, `fat`, `exfat` – metadata scanners for deleted entries
//! * `carve`   – signature-based deep scan
//! * `recover` – write recovered files out

pub mod carve;
pub mod device;
pub mod exfat;
pub mod fat;
pub mod model;
pub mod ntfs;
pub mod recover;
pub mod scan;
pub mod util;
pub mod volume;

pub use device::{image_info, list_devices, same_volume, Device, DeviceInfo};
pub use model::*;
pub use recover::{read_content, recover, write_content, RecoverReport};
pub use scan::{scan, ScanOptions, ScanResult};
