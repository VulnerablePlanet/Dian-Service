use chrono::{DateTime, FixedOffset, Utc};

/// Colombia usa UTC-05:00 todo el año (sin horario de verano).
const COLOMBIA_OFFSET_SECONDS: i32 = -5 * 3600;

/// Convierte un instante UTC a la fecha (`YYYY-MM-DD`) y hora (`HH:MM:SS-05:00`) de
/// Colombia, el formato que usan los documentos UBL de la DIAN.
pub fn colombia_date_time(instant: DateTime<Utc>) -> (String, String) {
    let offset = FixedOffset::east_opt(COLOMBIA_OFFSET_SECONDS).expect("valid fixed offset");
    let local = instant.with_timezone(&offset);
    (
        local.format("%Y-%m-%d").to_string(),
        local.format("%H:%M:%S%:z").to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn converts_utc_to_colombia_including_date_rollback() {
        let instant = Utc.with_ymd_and_hms(2026, 5, 24, 1, 0, 0).unwrap();
        let (date, time) = colombia_date_time(instant);
        assert_eq!(date, "2026-05-23");
        assert_eq!(time, "20:00:00-05:00");
    }

    #[test]
    fn keeps_same_day_during_colombian_daytime() {
        let instant = Utc.with_ymd_and_hms(2026, 5, 23, 15, 42, 7).unwrap();
        assert_eq!(
            colombia_date_time(instant),
            ("2026-05-23".to_string(), "10:42:07-05:00".to_string())
        );
    }
}
