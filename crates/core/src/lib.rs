//! Everything in WinterMiles that is pure computation: turning a GPX file into
//! a rideable route, simulating the ride, encoding the recording, decoding
//! sensor protocols and generating the landscape. No I/O happens here, so all
//! of it is unit-testable and independent of the app shell.

pub mod devices;
pub mod fit;
pub mod gamify;
pub mod gpx;
pub mod health;
pub mod physics;
pub mod ride;
pub mod types;
pub mod world;
