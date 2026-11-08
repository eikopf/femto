fn main() {
    for name in [
        "esp_idf_mbedtls_certificate_bundle",
        "esp_idf_mbedtls_have_time_date",
        "esp_idf_esp_tls_insecure",
        "esp_idf_mbedtls_ssl_proto_tls1_3",
        "esp_idf_bt_bluedroid_enabled",
        "esp_idf_bt_ble_42_scan_en",
        "esp_idf_bt_gattc_cache_nvs_flash",
        "esp_idf_bt_ble_smp_bond_nvs_flash",
    ] {
        println!("cargo::rustc-check-cfg=cfg({name})");
    }
    embuild::espidf::sysenv::output();
}
