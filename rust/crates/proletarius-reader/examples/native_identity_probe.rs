//! Read-only PC/SC identity smoke test. Subscriber values are never printed.
use proletarius_reader::{PcscReaderScanner, ReaderScanner, read_carrier_identity_auto};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let readers = PcscReaderScanner.scan()?;
    if readers.len() != 1 || !readers[0].card.as_str().eq("present") {
        return Err("probe requires exactly one present card".into());
    }
    let name = &readers[0].name;
    let carrier = read_carrier_identity_auto(name)?;
    let identity = &carrier.identity;
    if !identity.iccid.bytes().all(|digit| digit.is_ascii_digit())
        || !identity.imsi.bytes().all(|digit| digit.is_ascii_digit())
        || identity.mcc.len() != 3
        || (!carrier.gid1.is_empty()
            && (carrier.gid1.len() % 2 != 0
                || !carrier
                    .gid1
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_lowercase())))
        || (!carrier.gid2.is_empty()
            && (carrier.gid2.len() % 2 != 0
                || !carrier
                    .gid2
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_lowercase())))
    {
        return Err("native card identity has invalid shape".into());
    }
    println!(
        "reader=present usim_aid=found iccid=valid imsi=valid mnc=optional gid1={} gid2={} spn={}",
        if carrier.gid1.is_empty() {
            "absent"
        } else {
            "valid"
        },
        if carrier.gid2.is_empty() {
            "absent"
        } else {
            "valid"
        },
        if carrier.spn.is_empty() {
            "absent"
        } else {
            "valid"
        },
    );
    Ok(())
}
