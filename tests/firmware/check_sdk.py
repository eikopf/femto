"""Run the patched SDK's storage code on disposable host media with sanitizers."""
import argparse
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
PRELUDE = r'''
#include <assert.h>
#include <string.h>
typedef int gpio_num_t;
#define GPIO_NUM_NC -1
#define GPIO_IS_VALID_GPIO(pin) ((pin) >= 0 && (pin) < 49)
typedef struct { int cd, wp, clk, cmd, d0, d1, d2, d3, d4, d5, d6, d7; } sdmmc_slot_io_info_t;
typedef struct { sdmmc_slot_io_info_t slot_gpio_num; int slot_width; } slot_ctx_t;
static struct { slot_ctx_t slot_ctx[2]; } s_host_ctx;
static unsigned reset[49];
static void gpio_reset_pin(int pin) { reset[pin]++; }
'''
MAIN = r'''
int main(void) {
    s_host_ctx.slot_ctx[1].slot_width = 4;
    s_host_ctx.slot_ctx[1].slot_gpio_num = (sdmmc_slot_io_info_t){
        .cd=38, .wp=-1, .clk=41, .cmd=42, .d0=40, .d1=39, .d2=44, .d3=43 };
    sdmmc_host_deinit_slot_internal(0);
    sdmmc_host_deinit_slot_internal(1);
    assert(reset[0] == 0);
    for (int pin=38; pin<=44; pin++) assert(reset[pin] == 1);
    sdmmc_host_deinit_slot_internal(1);
    assert(reset[0] == 0);
    for (int pin=38; pin<=44; pin++) assert(reset[pin] == 1);
}
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--idf", type=Path, required=True)
    parser.add_argument("--sdkconfig", type=Path, required=True)
    args = parser.parse_args()
    fatfs = args.idf / "components/fatfs/src"
    compiler = os.environ.get("CC", "cc")
    with tempfile.TemporaryDirectory() as directory:
        tmp = Path(directory)
        (tmp / "freertos").mkdir()
        (tmp / "freertos/FreeRTOS.h").write_text("#define portTICK_PERIOD_MS 10\n")
        (tmp / "freertos/semphr.h").write_text("typedef void *SemaphoreHandle_t;\n")
        (tmp / "sdkconfig.h").write_bytes(args.sdkconfig.read_bytes())
        common = [compiler, "-std=c11", "-O1", "-g", "-fsanitize=address,undefined",
                  "-fno-omit-frame-pointer", "-I", str(tmp), "-I", str(fatfs)]
        subprocess.run(common + [str(ROOT / "tests/firmware/sdk/fatfs.c"),
                       str(fatfs / "ff.c"), str(fatfs / "ffunicode.c"), "-o", str(tmp / "fatfs")],
                       check=True, timeout=120)
        subprocess.run([str(tmp / "fatfs")], check=True, timeout=60)
        host = (args.idf / "components/esp_driver_sdmmc/src/sdmmc_host.c").read_text()
        start = host.index("static void reset_pin_if_valid(")
        end = host.index("esp_err_t sdmmc_host_deinit_slot(", start)
        body = host[start:end]
        guard = "    if (s_host_ctx.slot_ctx[slot].slot_width == 0) {\n        return;\n    }\n"
        assert guard in body
        for name, source, success in [("patched", body, True), ("original", body.replace(guard, ""), False)]:
            path = tmp / f"{name}.c"
            path.write_text(PRELUDE + source + MAIN)
            subprocess.run(common + [str(path), "-o", str(tmp / name)], check=True, timeout=60)
            result = subprocess.run([str(tmp / name)], capture_output=True, text=True, timeout=10)
            assert (result.returncode == 0) == success, result.stderr
            if not success:
                assert "reset[0] == 0" in result.stderr, result.stderr
        print("SDMMC cleanup: unused slots preserve GPIO0; original SDK regression reproduced")


if __name__ == "__main__":
    main()
