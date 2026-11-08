use esp_idf_svc::{
    fs::fatfs::Fatfs,
    hal::{
        gpio::*,
        sd::{
            SdCardDriver,
            mmc::{SDMMC1, SdMmcHostDriver},
        },
    },
    io::vfs::MountedFatfs,
};

pub type Storage = MountedFatfs<Fatfs<SdCardDriver<SdMmcHostDriver<'static>>>>;

#[allow(clippy::too_many_arguments)]
pub fn mount(
    slot: SDMMC1<'static>,
    cmd: Gpio42<'static>,
    clk: Gpio41<'static>,
    d0: Gpio40<'static>,
    d1: Gpio39<'static>,
    d2: Gpio44<'static>,
    d3: Gpio43<'static>,
    cd: Gpio38<'static>,
) -> Result<Storage, String> {
    let host = SdMmcHostDriver::new_4bits(
        slot,
        cmd,
        clk,
        d0,
        d1,
        d2,
        d3,
        Some(cd),
        None::<AnyInputPin>,
        &Default::default(),
    )
    .map_err(|e| e.to_string())?;
    let mut config = esp_idf_svc::hal::sd::config::Configuration::default();
    config.speed_khz = 20_000;
    config.command_timeout_ms = 1000;
    let card = SdCardDriver::new_mmc(host, &config).map_err(|e| e.to_string())?;
    let fat = Fatfs::new_sdcard(0, card).map_err(|e| e.to_string())?;
    MountedFatfs::mount(fat, "/sdcard", 8).map_err(|e| e.to_string())
}
