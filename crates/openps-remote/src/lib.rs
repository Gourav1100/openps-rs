//! PSN remote registration of a PS5 (specs `analysis/spec/09-remote-registration.md` and `09b-remote-registration-detail.md`).
//!
//! The layers, bottom up; each is a pure function over bytes or JSON so it can be tested without a console or Sony:
//! * [`keys`]: the console's `customData1` -> the register key, and the register request/response
//! * [`rudp`]: the small reliable-UDP layer the register request rides on
//! * [`punch`]: the 88-byte hole-punch packets
//! * [`signaling`]: the PSN session messages (OFFER / ACCEPT / RESULT) and notifications
pub mod keys;
pub mod punch;
pub mod rudp;
pub mod session;
pub mod signaling;
#[cfg(feature = "live")]
pub mod tls;
#[cfg(feature = "live")]
pub mod ws;
