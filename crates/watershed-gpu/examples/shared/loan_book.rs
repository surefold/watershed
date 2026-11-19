//! The made-up loan book the examples that read `credit_loss.xlsx` share.

/// The workbook, and the loan's cells in it: what is owed, the yearly rate,
/// the quarters left, the property's value, the chance of default in a year
/// as scored at first and today, and the days past due.
pub const WORKBOOK: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/shared/credit_loss.xlsx"
);
pub const LOAN: [&str; 7] = [
    "Loan!B4", "Loan!B5", "Loan!B6", "Loan!B7", "Loan!B8", "Loan!B9", "Loan!B10",
];

/// A small deterministic random number generator, so every run is the same.
pub struct Rng(pub u64);
impl Rng {
    pub fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
}

fn to_places(x: f64, places: i32) -> f64 {
    let m = 10f64.powi(places);
    (x * m).round() / m
}

/// One loan of a made-up book, as the seven numbers the workbook asks for.
pub fn made_up_loan(rng: &mut Rng) -> [f64; 7] {
    let owed = to_places(40_000.0 * (2.7 * rng.unit()).exp(), 2);
    let rate = to_places(0.02 + 0.05 * rng.unit(), 4);
    let quarters = (8 + (93.0 * rng.unit()) as u32) as f64;
    let value = to_places(owed / (0.2 + 0.9 * rng.unit().powf(0.7)), 0);
    let at_first = to_places(0.002 * (2.7 * rng.unit()).exp(), 5);
    // Most loans are scored about as they were; one in seven much worse.
    let drifted = match rng.unit() < 0.85 {
        true => 0.6 + 0.8 * rng.unit(),
        false => 2.0 + 6.0 * rng.unit(),
    };
    let now = to_places((at_first * drifted).min(0.6), 5);
    let late = rng.unit();
    let days = if late < 0.95 {
        0
    } else if late < 0.975 {
        1 + (30.0 * rng.unit()) as u32
    } else if late < 0.99 {
        31 + (59.0 * rng.unit()) as u32
    } else {
        90 + (200.0 * rng.unit()) as u32
    };
    [owed, rate, quarters, value, at_first, now, days as f64]
}
