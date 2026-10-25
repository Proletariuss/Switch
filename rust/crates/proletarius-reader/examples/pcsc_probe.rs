use proletarius_reader::{PcscReaderScanner, ReaderScanner};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let readers = PcscReaderScanner.scan()?;
    println!("readers={}", readers.len());
    for reader in readers {
        println!(
            "name={:?} card={} in_use={} muted={} atr_bytes={}",
            reader.name,
            reader.card.as_str(),
            reader.in_use,
            reader.muted,
            reader.atr.len()
        );
    }
    Ok(())
}
