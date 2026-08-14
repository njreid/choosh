//! `getrandom::SysRng` only implements the fallible
//! `rand_core::TryRng`/`TryCryptoRng` (OS randomness can, rarely, fail).
//! Every caller in this crate wants the ordinary infallible
//! `RngCore`/`CryptoRng` (panicking on the practically-never-occurring
//! failure) — notably `ed25519_dalek::SigningKey::generate`, which requires
//! `ed25519_dalek::rand_core::CryptoRng` specifically. `ed25519-dalek`'s own
//! docs recommend exactly this `getrandom`+`UnwrapErr` pairing (its
//! `rand_core` feature re-exports the identical `rand_core` release
//! `getrandom` depends on), so this wraps `SysRng` once here rather than
//! repeating it at every call site — and rather than going through the
//! separate `rand` crate, whose own `rand_core` dependency resolves to a
//! different, incompatible major version in this workspace's graph.

use getrandom::SysRng;
use getrandom::rand_core::UnwrapErr;

#[must_use]
pub fn os_rng() -> UnwrapErr<SysRng> {
    UnwrapErr(SysRng)
}
