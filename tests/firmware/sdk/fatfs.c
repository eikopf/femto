/* Exercise the pinned FatFs implementation on disposable memory-backed media. */
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "ff.h"
#include "diskio.h"

#if !FF_FS_EXFAT || FF_LFN_UNICODE != 2
#error "Tests require the Picco exFAT and UTF-8 configuration"
#endif

enum { SECTORS = 131072, SECTOR_BYTES = 512, PAYLOAD = 10013 };
static BYTE *media;
static unsigned writes;
PARTITION VolToPart[FF_VOLUMES];
DSTATUS disk_initialize(BYTE drive) { return drive ? STA_NODISK : 0; }
DSTATUS disk_status(BYTE drive) { return disk_initialize(drive); }
DRESULT disk_read(BYTE drive, BYTE *buf, LBA_t sector, UINT count) {
    if (drive || (uint64_t)sector + count > SECTORS) return RES_PARERR;
    memcpy(buf, media + (size_t)sector * SECTOR_BYTES, (size_t)count * SECTOR_BYTES);
    return RES_OK;
}
DRESULT disk_write(BYTE drive, const BYTE *buf, LBA_t sector, UINT count) {
    if (drive || (uint64_t)sector + count > SECTORS) return RES_PARERR;
    memcpy(media + (size_t)sector * SECTOR_BYTES, buf, (size_t)count * SECTOR_BYTES);
    writes++;
    return RES_OK;
}
DRESULT disk_ioctl(BYTE drive, BYTE cmd, void *buf) {
    if (drive) return RES_PARERR;
    switch (cmd) {
        case CTRL_SYNC: case CTRL_TRIM: return RES_OK;
        case GET_SECTOR_COUNT: *(LBA_t *)buf = SECTORS; return RES_OK;
        case GET_SECTOR_SIZE: *(WORD *)buf = SECTOR_BYTES; return RES_OK;
        case GET_BLOCK_SIZE: *(DWORD *)buf = 1; return RES_OK;
        default: return RES_PARERR;
    }
}
void *ff_memalloc(UINT size) { return malloc(size); }
void ff_memfree(void *ptr) { free(ptr); }
int ff_mutex_create(int volume) { (void)volume; return 1; }
void ff_mutex_delete(int volume) { (void)volume; }
int ff_mutex_take(int volume) { (void)volume; return 1; }
void ff_mutex_give(int volume) { (void)volume; }
DWORD get_fattime(void) { return (46u << 25) | (11u << 21) | (15u << 16); }

static BYTE pattern(unsigned offset) { return (BYTE)((offset * 73u + 19u) % 251u); }
static void check_range(FIL *file, unsigned offset, unsigned length) {
    BYTE buf[317];
    assert(f_lseek(file, offset) == FR_OK);
    for (unsigned checked = 0; checked < length;) {
        UINT count = length - checked < sizeof(buf) ? length - checked : sizeof(buf);
        UINT received = 0;
        assert(f_read(file, buf, count, &received) == FR_OK && received == count);
        for (unsigned i = 0; i < count; i++) assert(buf[i] == pattern(offset + checked + i));
        checked += count;
    }
}

static void roundtrip(BYTE format) {
    FATFS fs;
    FIL file;
    FILINFO info;
    BYTE work[4096];
    memset(media, 0, (size_t)SECTORS * SECTOR_BYTES);
    unsigned before = writes;
    assert(f_mount(&fs, "0:", 1) == FR_NO_FILESYSTEM);
    assert(writes == before); /* An unsuccessful mount must not format or repair media. */
    assert(f_mount(NULL, "0:", 0) == FR_OK);
    MKFS_PARM parameters = { .fmt = format | FM_SFD, .n_fat = 1, .n_root = 512 };
    assert(f_mkfs("0:", &parameters, work, sizeof(work)) == FR_OK);
    assert(f_mount(&fs, "0:", 1) == FR_OK);
    assert(f_mkdir("0:/femto") == FR_OK);
    const char *first = "0:/femto/reader-éléphant-كتاب.txt";
    const char *renamed = "0:/femto/renamed-éléphant-كتاب.txt";
    assert(f_open(&file, first, FA_WRITE | FA_CREATE_NEW) == FR_OK);
    for (unsigned offset = 0; offset < PAYLOAD;) {
        BYTE buf[701];
        UINT count = PAYLOAD - offset < sizeof(buf) ? PAYLOAD - offset : sizeof(buf);
        for (unsigned i = 0; i < count; i++) buf[i] = pattern(offset + i);
        UINT written = 0;
        assert(f_write(&file, buf, count, &written) == FR_OK && written == count);
        offset += count;
    }
    assert(f_sync(&file) == FR_OK);
    assert(f_close(&file) == FR_OK);
    assert(f_rename(first, renamed) == FR_OK);
    assert(f_mount(NULL, "0:", 0) == FR_OK);
    assert(f_mount(&fs, "0:", 1) == FR_OK);
    assert(f_stat(renamed, &info) == FR_OK && info.fsize == PAYLOAD);
    assert(f_open(&file, renamed, FA_READ | FA_WRITE) == FR_OK);
    check_range(&file, 4087, 1319);
    check_range(&file, 17, 512);
    check_range(&file, 0, PAYLOAD);
    assert(f_lseek(&file, 4097) == FR_OK);
    assert(f_truncate(&file) == FR_OK);
    assert(f_close(&file) == FR_OK);
    assert(f_stat(renamed, &info) == FR_OK && info.fsize == 4097);
    FF_DIR directory;
    assert(f_opendir(&directory, "0:/femto") == FR_OK);
    assert(f_readdir(&directory, &info) == FR_OK);
    assert(strcmp(info.fname, renamed + strlen("0:/femto/")) == 0);
    assert(f_closedir(&directory) == FR_OK);
    assert(f_unlink(renamed) == FR_OK);
    assert(f_unlink("0:/femto") == FR_OK);
    assert(f_mount(NULL, "0:", 0) == FR_OK);
}

int main(void) {
    media = calloc(SECTORS, SECTOR_BYTES);
    assert(media);
    roundtrip(FM_FAT);
    roundtrip(FM_FAT32);
    roundtrip(FM_EXFAT);
    free(media);
    puts("FAT16, FAT32 and exFAT: UTF-8 paths, write/sync/remount, seek, rename and truncate passed");
}
