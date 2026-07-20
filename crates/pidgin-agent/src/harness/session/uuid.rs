//! uuidv7 generator mirroring `packages/agent/src/harness/session/uuid.ts`.
//!
//! pi's generator keeps module-global monotonic state (`lastTimestamp`,
//! `sequence`). To make the golden vectors in `session-uuid.test.ts`
//! reproducible, the deterministic core is [`Uuidv7Generator::generate`],
//! which takes the millisecond timestamp and the 16 random bytes as inputs.
//! [`uuidv7`] wraps a process-global generator fed by the real clock and RNG.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Monotonic uuidv7 state. `last_timestamp` starts at [`i64::MIN`], the stand-in
/// for pi's `-Infinity` sentinel (no real epoch-millis value collides with it).
#[derive(Debug)]
pub struct Uuidv7Generator {
    last_timestamp: i64,
    sequence: u32,
}

impl Default for Uuidv7Generator {
    fn default() -> Self {
        Self::new()
    }
}

impl Uuidv7Generator {
    pub fn new() -> Self {
        Self {
            last_timestamp: i64::MIN,
            sequence: 0,
        }
    }

    /// Produce the next uuidv7 for the given millisecond `timestamp` and 16
    /// random bytes, advancing the monotonic sequence exactly as pi does.
    pub fn generate(&mut self, timestamp: i64, random: [u8; 16]) -> String {
        if timestamp > self.last_timestamp {
            self.sequence = (u32::from(random[6]) << 24)
                | (u32::from(random[7]) << 16)
                | (u32::from(random[8]) << 8)
                | u32::from(random[9]);
            self.last_timestamp = timestamp;
        } else {
            self.sequence = self.sequence.wrapping_add(1);
            if self.sequence == 0 {
                self.last_timestamp += 1;
            }
        }

        let ts = self.last_timestamp;
        let seq = self.sequence;
        let mut bytes = [0u8; 16];
        bytes[0] = ((ts >> 40) & 0xff) as u8;
        bytes[1] = ((ts >> 32) & 0xff) as u8;
        bytes[2] = ((ts >> 24) & 0xff) as u8;
        bytes[3] = ((ts >> 16) & 0xff) as u8;
        bytes[4] = ((ts >> 8) & 0xff) as u8;
        bytes[5] = (ts & 0xff) as u8;
        bytes[6] = 0x70 | ((seq >> 28) & 0x0f) as u8;
        bytes[7] = ((seq >> 20) & 0xff) as u8;
        bytes[8] = 0x80 | ((seq >> 14) & 0x3f) as u8;
        bytes[9] = ((seq >> 6) & 0xff) as u8;
        bytes[10] = (((seq & 0x3f) << 2) as u8) | (random[10] & 0x03);
        bytes[11] = random[11];
        bytes[12] = random[12];
        bytes[13] = random[13];
        bytes[14] = random[14];
        bytes[15] = random[15];

        format_uuid(&bytes)
    }
}

fn format_uuid(bytes: &[u8; 16]) -> String {
    let mut hex = String::with_capacity(36);
    for (i, byte) in bytes.iter().enumerate() {
        if i == 4 || i == 6 || i == 8 || i == 10 {
            hex.push('-');
        }
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

static GLOBAL: Mutex<Uuidv7Generator> = Mutex::new(Uuidv7Generator {
    last_timestamp: i64::MIN,
    sequence: 0,
});

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn fill_random(bytes: &mut [u8; 16]) {
    // A tiny xorshift seeded from the clock. The random tail only needs to be
    // unpredictable enough to avoid short-id collisions; ids are not secrets.
    let mut state = now_millis() as u64 ^ 0x9e37_79b9_7f4a_7c15;
    let addr = bytes.as_ptr() as u64;
    state ^= addr.rotate_left(17);
    for b in bytes.iter_mut() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *b = (state & 0xff) as u8;
    }
}

/// Generate a uuidv7 string using the process-global monotonic state.
pub fn uuidv7() -> String {
    let mut random = [0u8; 16];
    fill_random(&mut random);
    let timestamp = now_millis();
    GLOBAL
        .lock()
        .expect("uuidv7 state poisoned")
        .generate(timestamp, random)
}
