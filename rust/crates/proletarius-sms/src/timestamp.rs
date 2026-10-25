//! Validated SMS semi-octet calendar fields, without a platform timezone dependency.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Timestamp {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub offset_minutes: i16,
}
#[derive(Debug, PartialEq, Eq)]
pub struct InvalidTimestamp;

fn decimal(value: u8) -> Result<u8, InvalidTimestamp> {
    let low = value & 15;
    let high = value >> 4;
    if low > 9 || high > 9 {
        return Err(InvalidTimestamp);
    }
    Ok(low * 10 + high)
}

pub fn parse(bytes: &[u8]) -> Result<Timestamp, InvalidTimestamp> {
    if bytes.len() != 7 {
        return Err(InvalidTimestamp);
    }
    let year = decimal(bytes[0])?;
    let year = if year >= 90 {
        1900 + u16::from(year)
    } else {
        2000 + u16::from(year)
    };
    let month = decimal(bytes[1])?;
    let day = decimal(bytes[2])?;
    let hour = decimal(bytes[3])?;
    let minute = decimal(bytes[4])?;
    let second = decimal(bytes[5])?;
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => return Err(InvalidTimestamp),
    };
    if day == 0 || day > days || hour > 23 || minute > 59 || second > 59 {
        return Err(InvalidTimestamp);
    }
    let quarters = decimal(bytes[6] & !8)?;
    if quarters > 56 {
        return Err(InvalidTimestamp);
    }
    let sign = if bytes[6] & 8 != 0 { -1 } else { 1 };
    Ok(Timestamp {
        year,
        month,
        day,
        hour,
        minute,
        second,
        offset_minutes: i16::from(quarters) * 15 * sign,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timezone_and_year_pivot_match_existing_decoder() {
        let value = parse(&[0x62, 0x90, 0x60, 0x51, 0x44, 0, 0x23]).unwrap();
        assert_eq!(value.year, 2026);
        assert_eq!(value.offset_minutes, 480);
        assert_eq!(
            parse(&[0x99, 0x10, 0x10, 0, 0, 0, 0x88])
                .unwrap()
                .offset_minutes,
            -120
        );
        assert_eq!(parse(&[0x99, 0x10, 0x10, 0, 0, 0, 0]).unwrap().year, 1999);
    }
    #[test]
    fn rejects_impossible_dates_and_bad_bcd() {
        assert!(parse(&[0x42, 0x20, 0x92, 0, 0, 0, 0]).is_ok());
        assert!(parse(&[0x52, 0x20, 0x92, 0, 0, 0, 0]).is_err());
        assert!(parse(&[0x62, 0x40, 0x13, 0, 0, 0, 0]).is_err());
        assert!(parse(&[0xff, 0x10, 0x10, 0, 0, 0, 0]).is_err());
        assert!(parse(&[0x62, 0x10, 0x10, 0, 0, 0, 0x75]).is_err());
    }
}
