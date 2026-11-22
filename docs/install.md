# Installing femto on a BOOX Picco

Femto replaces the Picco's reading application and leaves everything else in flash alone: the bootloader, the partition table and BOOX's recovery application stay as shipped. The first installation is done over USB from a computer. Later updates need only the SD card.

BOOX does not publish the Picco's firmware. A backup of your own device is the only way back to the stock reader, so make one before installing.

## What you need

- A BOOX Picco and a USB-C data cable.
- A computer with [esptool](https://docs.espressif.com/projects/esptool/) 5 or later. With Nix, `nix shell nixpkgs#esptool` provides it.
- `femto-picco.bin` from the [latest release](https://github.com/eikopf/femto/releases/latest).

Femto has been run on one Picco, an ESP32-S3 with 16 MB of flash. The commands below assume the stock partition layout, with the application at offset `0xE0000`. A device that has been repartitioned by other firmware needs a different offset.

## 1. Back up the device

Connect the Picco. It appears as an Espressif USB JTAG/serial device, `/dev/ttyACM0` on most Linux systems.

```sh
esptool --chip esp32s3 --port /dev/ttyACM0 read-flash 0 0x1000000 picco-backup.bin
```

This reads all 16 MB and takes a few minutes. Keep the file somewhere safe. Reading it a second time and comparing the two files confirms the backup is sound.

## 2. Install femto

```sh
esptool --chip esp32s3 --port /dev/ttyACM0 --after no-reset \
  write-flash --flash-mode keep --flash-freq keep --flash-size keep \
  0xe0000 femto-picco.bin
esptool --chip esp32s3 --port /dev/ttyACM0 --before no-reset --after watchdog-reset flash-id
```

The second command restarts the reader. An ordinary reset leaves this board waiting in its USB download mode, which is why the restart is a separate step. From a checkout of this repository, `./scripts/picco-flash femto-picco.bin` runs both commands.

## 3. Set up

Femto keeps its data in a `femto` directory on the SD card. Copy EPUB or plain-text books anywhere below it and choose Library, Rescan folder. Wi-Fi, a catalog and KOReader sync are set up on the device under Settings, Network and services.

## Updating

With Wi-Fi set up, choose Settings, Check for updates, then Download update when a newer release exists. Without Wi-Fi, copy a newer `femto-picco.bin` to the card as `update/firmware.bin`. Either way, Settings then shows Install update. The reader restarts into BOOX's recovery application, which installs the image and shows `Update complete`; tap Restart. Keep the reader charged or plugged in while it installs. [Updates](picco-firmware.md#updates) describes the mechanism.

## Returning to the stock firmware

Write the backup's application back to the same place:

```sh
dd if=picco-backup.bin of=stock-app.bin bs=65536 skip=14 count=231
esptool --chip esp32s3 --port /dev/ttyACM0 --after no-reset \
  write-flash --flash-mode keep --flash-freq keep --flash-size keep \
  0xe0000 stock-app.bin
esptool --chip esp32s3 --port /dev/ttyACM0 --before no-reset --after watchdog-reset flash-id
```

This has not been tested: the stock application has not been restored on the device femto was developed on.
