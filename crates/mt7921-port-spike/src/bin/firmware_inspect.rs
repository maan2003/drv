use mt7921_port_spike::{Firmware, Patch};
use std::process::Command;

const PATCH_PATH: &str =
    "/run/current-system/firmware/mediatek/WIFI_MT7961_patch_mcu_1_2_hdr.bin.zst";
const RAM_PATH: &str = "/run/current-system/firmware/mediatek/WIFI_RAM_CODE_MT7961_1.bin.zst";

fn main() {
    if let Err(message) = run() {
        eprintln!("mt7921-firmware-inspect: {message}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let patch_bytes = decompress(PATCH_PATH)?;
    let ram_bytes = decompress(RAM_PATH)?;
    let patch = Patch::parse(&patch_bytes).map_err(|error| format!("patch format: {error:?}"))?;
    let ram = Firmware::parse(&ram_bytes).map_err(|error| format!("RAM format: {error:?}"))?;
    if patch.header.platform != b"ALPS"
        || patch.header.hardware_software_version != 0x8a10_8a10
        || patch.region_count() != 1
    {
        return Err("patch metadata does not match MT7961_1".into());
    }
    let section = patch.sections().next().ok_or("patch has no section")?;
    if section.address != 0x0090_0000 || section.payload.len() != 0x16780 {
        return Err("patch download section does not match MT7961_1".into());
    }
    if ram.trailer.chip_id != 0x0d
        || ram.trailer.eco_code != 1
        || ram.trailer.format_version != 2
        || ram.trailer.format_flag != 1
        || ram.region_count() != 5
    {
        return Err("RAM trailer does not match MT7961_1".into());
    }
    let downloadable = ram
        .regions()
        .filter(|region| region.is_downloadable())
        .count();
    let clc = ram.regions().filter(|region| region.is_clc()).count();
    println!(
        "{{\"artifact\":\"{}\",\"bytes\":{},\"platform\":\"{}\",\"hardware_software_version\":\"{:#010x}\",\"build_date\":\"{}\",\"regions\":{},\"download_address\":\"{:#010x}\",\"download_bytes\":{}}}",
        PATCH_PATH,
        patch_bytes.len(),
        text(patch.header.platform),
        patch.header.hardware_software_version,
        text(patch.header.build_date),
        patch.region_count(),
        section.address,
        section.payload.len(),
    );
    println!(
        "{{\"artifact\":\"{}\",\"bytes\":{},\"chip_id\":\"{:#04x}\",\"eco_code\":{},\"format_version\":{},\"format_flag\":{},\"firmware_version\":\"{}\",\"build_date\":\"{}\",\"regions\":{},\"downloadable_regions\":{},\"clc_regions\":{},\"crc\":\"{:#010x}\"}}",
        RAM_PATH,
        ram_bytes.len(),
        ram.trailer.chip_id,
        ram.trailer.eco_code,
        ram.trailer.format_version,
        ram.trailer.format_flag,
        text(ram.trailer.firmware_version),
        text(ram.trailer.build_date),
        ram.region_count(),
        downloadable,
        clc,
        ram.trailer.crc,
    );
    Ok(())
}

fn decompress(path: &str) -> Result<Vec<u8>, String> {
    let output = Command::new("zstdcat")
        .arg(path)
        .output()
        .map_err(|error| format!("run zstdcat for {path}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "zstdcat {path}: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(output.stdout)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_end_matches(['\0', '\n'])
        .to_owned()
}
