//! Tiny non-cryptographic RNG (xorshift64*), used for `@` file-name tokens.

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    /// Seeded from the high-resolution clock and the stack address.
    pub fn from_entropy() -> Self {
        let mut t: i64 = 0;
        // SAFETY: valid out pointer.
        unsafe {
            let _ = windows::Win32::System::Performance::QueryPerformanceCounter(&mut t);
        }
        let local = 0u8;
        Self::new((t as u64) ^ ((&local as *const u8 as u64).rotate_left(32)) ^ 0x9E37_79B9_7F4A_7C15)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }
}
