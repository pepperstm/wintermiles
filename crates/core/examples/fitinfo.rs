//! Prints a summary of a FIT file using an independent decoder — handy for
//! checking what WinterMiles recorded.
//!
//!     cargo run -p wintermiles-core --example fitinfo -- path/to/ride.fit

use fitparser::profile::MesgNum;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: fitinfo <file.fit>")?;
    let records = fitparser::from_bytes(&std::fs::read(&path)?)?;
    let show = |r: &fitparser::FitDataRecord| r.fields().iter().map(|f| format!("{}={}", f.name(), f.value())).collect::<Vec<_>>().join(" ");
    let of = |kind: MesgNum| records.iter().filter(move |r| r.kind() == kind);
    println!("{path}: {} data records, {} laps", of(MesgNum::Record).count(), of(MesgNum::Lap).count());
    if let Some(s) = of(MesgNum::Session).next() {
        println!("session: {}", show(s));
    }
    if let (Some(first), Some(last)) = (of(MesgNum::Record).next(), of(MesgNum::Record).last()) {
        println!("first:   {}", show(first));
        println!("last:    {}", show(last));
    }
    Ok(())
}
