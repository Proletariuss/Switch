//! Non-persistent native PC/SC smoke test. The transaction serializes its
//! APDU sequence with other shared owners.
use proletarius_reader::{PcscReaderScanner, ReaderScanner, session::PcscSession};
use proletarius_sim::exchange_apdu;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let readers = PcscReaderScanner.scan()?;
    println!("reader_count={}", readers.len());
    if readers.len() != 1 {
        return Err("probe requires exactly one reader; no card was connected".into());
    }
    println!("card_presence={}", readers[0].card.as_str());
    if readers[0].muted {
        return Err("reader is muted; no card was connected".into());
    }
    let mut session = PcscSession::connect(&readers[0].name)?;
    let mut transaction = session.transaction()?;
    let outcome = exchange_apdu(&mut transaction, &[0, 0xa4, 0, 4, 2, 0x3f, 0]);
    drop(transaction);
    let close = session.close();
    let response = outcome?;
    close?;
    response.success_data()?;
    // Never print APDU data, ATR, ICCID, IMSI, AKA responses or reader names.
    println!("shared_connect=ok transaction=ok select_mf=ok close=ok");
    Ok(())
}
