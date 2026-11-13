//! 乱数は OS の CSPRNG（getrandom）だけを使う。
use crate::{Error, Result};

pub fn fill(buf: &mut [u8]) -> Result<()> {
    getrandom::fill(buf).map_err(|_| Error::Rng)
}

pub fn array<const N: usize>() -> Result<[u8; N]> {
    let mut a = [0u8; N];
    fill(&mut a)?;
    Ok(a)
}

/// 0..n の一様乱数（棄却サンプリングで偏りをなくす）。
pub fn below(n: u32) -> Result<u32> {
    assert!(n > 0);
    let zone = u32::MAX - (u32::MAX % n);
    loop {
        let v = u32::from_le_bytes(array::<4>()?);
        if v < zone {
            return Ok(v % n);
        }
    }
}
