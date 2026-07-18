//! Ports `test/harness/session-uuid.test.ts`. The golden strings pin the
//! uuidv7 spec, including monotonic sequencing within a millisecond.

use atilla_agent::harness::session::Uuidv7Generator;

const TIMESTAMP: i64 = 0x0123_4567_89ab;

fn matches_layout(uuid: &str) -> bool {
    // ^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$
    let parts: Vec<&str> = uuid.split('-').collect();
    if parts.len() != 5 {
        return false;
    }
    let lens = [8, 4, 4, 4, 12];
    for (part, len) in parts.iter().zip(lens) {
        if part.len() != len
            || !part
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return false;
        }
    }
    parts[2].starts_with('7') && matches!(parts[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b')
}

fn parse_timestamp(uuid: &str) -> i64 {
    let hex: String = uuid.chars().filter(|c| *c != '-').take(12).collect();
    i64::from_str_radix(&hex, 16).unwrap()
}

#[test]
fn uses_rfc_9562_layout_and_preserves_monotonic_order() {
    let mut generator = Uuidv7Generator::new();
    let random_values = [
        [
            0, 0, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xfe, 0x01, 0x11, 0x22, 0x33, 0x44, 0x55,
        ],
        [0u8; 16],
        [0u8; 16],
    ];

    let first = generator.generate(TIMESTAMP, random_values[0]);
    let second = generator.generate(TIMESTAMP, random_values[1]);
    let third = generator.generate(TIMESTAMP, random_values[2]);

    assert_eq!(first, "01234567-89ab-7fff-bfff-f91122334455");
    assert_eq!(second, "01234567-89ab-7fff-bfff-fc0000000000");
    assert_eq!(third, "01234567-89ac-7000-8000-000000000000");
    assert!(matches_layout(&first));
    assert!(matches_layout(&second));
    assert!(matches_layout(&third));
    assert_eq!(parse_timestamp(&first), TIMESTAMP);
    assert_eq!(parse_timestamp(&second), TIMESTAMP);
    assert_eq!(parse_timestamp(&third), TIMESTAMP + 1);
    assert!(first < second);
    assert!(second < third);
}

#[test]
fn global_uuidv7_matches_layout() {
    let uuid = atilla_agent::harness::session::uuidv7();
    assert!(matches_layout(&uuid), "unexpected uuid: {uuid}");
}
