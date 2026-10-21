use vosu_tool_difficulty::{Params, calculate, checksum};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    for path in std::env::args_os().skip(1) {
        let bytes = std::fs::read(&path)?;
        let result = calculate(&bytes, Params::new(1.0, None)?)?;
        println!(
            "{}\t{}\t{:.6}",
            std::path::Path::new(&path).display(),
            checksum(&bytes),
            result.msd.ok_or("not supported")?.overall
        );
    }
    Ok(())
}
