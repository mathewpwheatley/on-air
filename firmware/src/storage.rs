//! Settings in the flash's `settings` partition (see `partitions.csv`).
//! `sequential-storage` appends each save and erases pages only when they
//! fill, which spreads the wear; the settings are written rarely anyway.

use core::ops::Range;

use embassy_embedded_hal::adapter::BlockingAsync;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use esp_bootloader_esp_idf::partitions::{PARTITION_TABLE_MAX_LEN, read_partition_table};
use esp_hal::peripherals::FLASH;
use esp_storage::FlashStorage;
use log::{info, warn};
use on_air_core::settings::Settings;
use sequential_storage::cache::{Cache, Uncached};
use sequential_storage::map::{MapConfig, MapStorage};
use static_cell::StaticCell;

const PARTITION_LABEL: &str = "settings";
/// The one key in the map: the whole settings blob.
const SETTINGS_KEY: u8 = 0;
/// Room for the serialized settings, rounded up to the flash word size.
/// They're a few hundred bytes; the refresh token is the longest part.
const BUFFER_BYTES: usize = 1024;

type Flash = BlockingAsync<FlashStorage<'static>>;
type Map = MapStorage<u8, Flash, Cache<Uncached, Uncached, Uncached, u8>>;

pub struct SettingsStore {
    inner: Mutex<CriticalSectionRawMutex, Inner>,
}

struct Inner {
    map: Map,
    settings: Settings,
}

impl SettingsStore {
    /// Find the settings partition and load what's there. `None` when the
    /// partition is missing, e.g. flashed without `partitions.csv`.
    pub async fn open(flash: FLASH<'static>) -> Option<&'static SettingsStore> {
        static STORE: StaticCell<SettingsStore> = StaticCell::new();
        let mut flash = FlashStorage::new(flash);
        let range = find_partition(&mut flash)?;
        let config = MapConfig::try_new(range.clone())
            .inspect_err(|_| warn!("storage: the settings partition isn't page-aligned"))
            .ok()?;
        let mut map = MapStorage::new(BlockingAsync::new(flash), config, Cache::new_uncached());

        let mut buffer = [0u8; BUFFER_BYTES];
        let settings = match map.fetch_item::<&[u8]>(&mut buffer, &SETTINGS_KEY).await {
            Ok(Some(bytes)) => Settings::from_bytes(bytes).unwrap_or_else(|error| {
                warn!("storage: saved settings unreadable ({error:?}); starting fresh");
                Settings::default()
            }),
            Ok(None) => {
                info!("storage: nothing saved yet");
                Settings::default()
            }
            Err(error) => {
                warn!("storage: couldn't read settings: {error:?}");
                Settings::default()
            }
        };
        info!(
            "storage: settings partition at {:#x}, {} KB",
            range.start,
            (range.end - range.start) / 1024
        );
        Some(STORE.init(SettingsStore {
            inner: Mutex::new(Inner { map, settings }),
        }))
    }

    /// A copy of the current settings.
    pub async fn get(&self) -> Settings {
        self.inner.lock().await.settings.clone()
    }

    /// Change the settings and save them, if anything changed.
    pub async fn update(&self, change: impl FnOnce(&mut Settings)) {
        let mut inner = self.inner.lock().await;
        let mut settings = inner.settings.clone();
        change(&mut settings);
        if settings == inner.settings {
            return;
        }
        let bytes = settings.to_bytes();
        let mut buffer = [0u8; BUFFER_BYTES];
        match inner
            .map
            .store_item(&mut buffer, &SETTINGS_KEY, &bytes.as_slice())
            .await
        {
            Ok(()) => {
                info!("storage: settings saved ({} bytes)", bytes.len());
                inner.settings = settings;
            }
            Err(error) => warn!("storage: couldn't save settings: {error:?}"),
        }
    }
}

/// The flash range of the partition labelled `settings`.
fn find_partition(flash: &mut FlashStorage<'static>) -> Option<Range<u32>> {
    let mut table = [0u8; PARTITION_TABLE_MAX_LEN];
    let table = read_partition_table(flash, &mut table)
        .inspect_err(|error| warn!("storage: couldn't read the partition table: {error:?}"))
        .ok()?;
    let entry = table.iter().find(|p| p.label_as_str() == PARTITION_LABEL);
    if entry.is_none() {
        warn!("storage: no `{PARTITION_LABEL}` partition; flash with `just flash`");
    }
    let entry = entry?;
    Some(entry.offset()..entry.offset() + entry.len())
}
